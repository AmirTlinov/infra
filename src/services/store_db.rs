use crate::errors::ToolError;
use crate::utils::paths::{ensure_dir_exists, resolve_store_db_path};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct StoreRecord {
    pub key: String,
    pub value: Value,
    pub source: Option<String>,
    pub deleted: bool,
}

#[derive(Clone)]
pub struct StoreDb {
    path: PathBuf,
}

impl StoreDb {
    pub fn new() -> Result<Self, ToolError> {
        let path = resolve_store_db_path();
        ensure_dir_exists(&path).map_err(|err| {
            ToolError::internal(format!("Failed to prepare store DB dir: {}", err))
        })?;
        let db = Self { path };
        db.init()?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn open(&self) -> Result<Connection, ToolError> {
        let conn = Connection::open(&self.path)
            .map_err(|err| ToolError::internal(format!("Failed to open store DB: {}", err)))?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(|err| {
            ToolError::internal(format!("Failed to configure store DB timeout: {}", err))
        })?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|err| ToolError::internal(format!("Failed to enable WAL mode: {}", err)))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|err| {
                ToolError::internal(format!("Failed to configure store DB sync mode: {}", err))
            })?;
        conn.pragma_update(None, "foreign_keys", 1).map_err(|err| {
            ToolError::internal(format!("Failed to enable foreign keys: {}", err))
        })?;
        Ok(conn)
    }

    fn init(&self) -> Result<(), ToolError> {
        let conn = self.open()?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS store_entries (
                namespace TEXT NOT NULL,
                key TEXT NOT NULL,
                payload TEXT,
                source TEXT,
                deleted INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL,
                PRIMARY KEY(namespace, key)
            );
            CREATE INDEX IF NOT EXISTS idx_store_entries_namespace
                ON store_entries(namespace, deleted, key);

            CREATE TABLE IF NOT EXISTS store_imports (
                namespace TEXT NOT NULL,
                import_key TEXT NOT NULL,
                imported_at TEXT NOT NULL,
                PRIMARY KEY(namespace, import_key)
            );
            "#,
        )
        .map_err(|err| ToolError::internal(format!("Failed to initialize store DB: {}", err)))?;
        Ok(())
    }

    pub fn has_import(&self, namespace: &str, import_key: &str) -> Result<bool, ToolError> {
        let conn = self.open()?;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM store_imports WHERE namespace = ?1 AND import_key = ?2 LIMIT 1",
                params![namespace, import_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| ToolError::internal(format!("Failed to read store imports: {}", err)))?;
        Ok(found.is_some())
    }

    pub fn has_imported(&self, namespace: &str, import_key: &str) -> Result<bool, ToolError> {
        self.has_import(namespace, import_key)
    }

    pub fn mark_imported(&self, namespace: &str, import_key: &str) -> Result<(), ToolError> {
        let conn = self.open()?;
        conn.execute(
            r#"
            INSERT INTO store_imports(namespace, import_key, imported_at)
            VALUES(?1, ?2, ?3)
            ON CONFLICT(namespace, import_key) DO UPDATE SET imported_at = excluded.imported_at
            "#,
            params![namespace, import_key, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|err| ToolError::internal(format!("Failed to update store imports: {}", err)))?;
        Ok(())
    }

    pub fn get(&self, namespace: &str, key: &str) -> Result<Option<StoreRecord>, ToolError> {
        let conn = self.open()?;
        let record = conn
            .query_row(
                r#"
                SELECT key, payload, source, deleted
                FROM store_entries
                WHERE namespace = ?1 AND key = ?2 AND deleted = 0
                LIMIT 1
                "#,
                params![namespace, key],
                row_to_record,
            )
            .optional()
            .map_err(|err| ToolError::internal(format!("Failed to read store entry: {}", err)))?;
        Ok(record)
    }

    pub fn bucket_count(&self, namespace: &str) -> Result<usize, ToolError> {
        Ok(self.list(namespace)?.len())
    }

    pub fn namespace_is_empty(&self, namespace: &str) -> Result<bool, ToolError> {
        Ok(self.bucket_count(namespace)? == 0)
    }

    pub fn get_any(&self, namespace: &str, key: &str) -> Result<Option<StoreRecord>, ToolError> {
        let conn = self.open()?;
        let record = conn
            .query_row(
                r#"
                SELECT key, payload, source, deleted
                FROM store_entries
                WHERE namespace = ?1 AND key = ?2
                LIMIT 1
                "#,
                params![namespace, key],
                row_to_record,
            )
            .optional()
            .map_err(|err| ToolError::internal(format!("Failed to read store entry: {}", err)))?;
        Ok(record)
    }

    pub fn list(&self, namespace: &str) -> Result<Vec<StoreRecord>, ToolError> {
        self.list_internal(namespace, false)
    }

    pub fn list_all(&self, namespace: &str) -> Result<Vec<StoreRecord>, ToolError> {
        self.list_internal(namespace, true)
    }

    fn list_internal(
        &self,
        namespace: &str,
        include_deleted: bool,
    ) -> Result<Vec<StoreRecord>, ToolError> {
        let conn = self.open()?;
        let sql = if include_deleted {
            r#"
            SELECT key, payload, source, deleted
            FROM store_entries
            WHERE namespace = ?1
            ORDER BY key ASC
            "#
        } else {
            r#"
            SELECT key, payload, source, deleted
            FROM store_entries
            WHERE namespace = ?1 AND deleted = 0
            ORDER BY key ASC
            "#
        };
        let mut stmt = conn.prepare(sql).map_err(|err| {
            ToolError::internal(format!("Failed to prepare store list query: {}", err))
        })?;
        let rows = stmt
            .query_map(params![namespace], row_to_record)
            .map_err(|err| ToolError::internal(format!("Failed to list store entries: {}", err)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|err| {
                ToolError::internal(format!("Failed to decode store entry: {}", err))
            })?);
        }
        Ok(out)
    }

    pub fn upsert(
        &self,
        namespace: &str,
        key: &str,
        value: &Value,
        source: Option<&str>,
    ) -> Result<(), ToolError> {
        let conn = self.open()?;
        let payload = serde_json::to_string(value).map_err(|err| {
            ToolError::internal(format!("Failed to serialize store value: {}", err))
        })?;
        conn.execute(
            r#"
            INSERT INTO store_entries(namespace, key, payload, source, deleted, updated_at)
            VALUES(?1, ?2, ?3, ?4, 0, ?5)
            ON CONFLICT(namespace, key) DO UPDATE SET
                payload = excluded.payload,
                source = excluded.source,
                deleted = 0,
                updated_at = excluded.updated_at
            "#,
            params![
                namespace,
                key,
                payload,
                source.unwrap_or("local"),
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|err| ToolError::internal(format!("Failed to upsert store entry: {}", err)))?;
        Ok(())
    }

    pub fn delete(&self, namespace: &str, key: &str) -> Result<bool, ToolError> {
        let conn = self.open()?;
        let changed = conn
            .execute(
                "DELETE FROM store_entries WHERE namespace = ?1 AND key = ?2",
                params![namespace, key],
            )
            .map_err(|err| ToolError::internal(format!("Failed to delete store entry: {}", err)))?;
        Ok(changed > 0)
    }

    pub fn tombstone(
        &self,
        namespace: &str,
        key: &str,
        source: Option<&str>,
    ) -> Result<(), ToolError> {
        let conn = self.open()?;
        conn.execute(
            r#"
            INSERT INTO store_entries(namespace, key, payload, source, deleted, updated_at)
            VALUES(?1, ?2, NULL, ?3, 1, ?4)
            ON CONFLICT(namespace, key) DO UPDATE SET
                payload = NULL,
                source = excluded.source,
                deleted = 1,
                updated_at = excluded.updated_at
            "#,
            params![
                namespace,
                key,
                source.unwrap_or("local"),
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|err| ToolError::internal(format!("Failed to tombstone store entry: {}", err)))?;
        Ok(())
    }

    pub fn clear_namespace(&self, namespace: &str) -> Result<(), ToolError> {
        let conn = self.open()?;
        conn.execute(
            "DELETE FROM store_entries WHERE namespace = ?1",
            params![namespace],
        )
        .map_err(|err| ToolError::internal(format!("Failed to clear store namespace: {}", err)))?;
        Ok(())
    }
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoreRecord> {
    let payload: Option<String> = row.get(1)?;
    let value = match payload {
        Some(raw) => serde_json::from_str(&raw).unwrap_or(Value::Null),
        None => Value::Null,
    };
    Ok(StoreRecord {
        key: row.get(0)?,
        value,
        source: row.get(2)?,
        deleted: row.get::<_, i64>(3)? != 0,
    })
}
