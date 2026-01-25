use crate::constants::{limits as limit_constants, network as network_constants};
use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::profile::ProfileService;
use crate::services::project_resolver::ProjectResolver;
use crate::services::secret_ref::SecretRefResolver;
use crate::services::validation::Validation;
use crate::utils::sql::{build_where_clause, normalize_table_context, quote_qualified_identifier};
use crate::utils::tool_errors::unknown_action_error;
use async_trait::async_trait;
use bb8::Pool;
use bb8_postgres::PostgresConnectionManager;
use dashmap::DashMap;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio_postgres::types::{Json, ToSql, Type};
use tokio_postgres::{Config, GenericClient, NoTls, Row};

const PG_PROFILE_TYPE: &str = "postgresql";
const PG_ACTIONS: &[&str] = &[
    "profile_upsert",
    "profile_get",
    "profile_list",
    "profile_delete",
    "profile_test",
    "query",
    "batch",
    "transaction",
    "insert",
    "insert_bulk",
    "update",
    "delete",
    "select",
    "count",
    "exists",
    "export",
    "catalog_tables",
    "catalog_columns",
    "database_info",
];

#[derive(Clone)]
pub struct PostgresManager {
    logger: Logger,
    validation: Validation,
    profile_service: Arc<ProfileService>,
    project_resolver: Option<Arc<ProjectResolver>>,
    secret_ref_resolver: Option<Arc<SecretRefResolver>>,
    pools: Arc<DashMap<String, Pool<PostgresConnectionManager<NoTls>>>>,
}

pub(crate) struct ExportStream {
    pub reader: DuplexStream,
    pub completion: tokio::task::JoinHandle<Result<Value, ToolError>>,
}

#[derive(Clone, Default)]
struct PoolOptions {
    max_size: Option<u32>,
    min_idle: Option<u32>,
    idle_timeout_ms: Option<u64>,
    connection_timeout_ms: Option<u64>,
}

#[derive(Clone)]
struct ResolvedConnection {
    config: Config,
    pool_options: PoolOptions,
    profile_name: Option<String>,
    key_seed: String,
}

impl PostgresManager {
    pub fn new(
        logger: Logger,
        validation: Validation,
        profile_service: Arc<ProfileService>,
        project_resolver: Option<Arc<ProjectResolver>>,
        secret_ref_resolver: Option<Arc<SecretRefResolver>>,
    ) -> Self {
        Self {
            logger: logger.child("psql"),
            validation,
            profile_service,
            project_resolver,
            secret_ref_resolver,
            pools: Arc::new(DashMap::new()),
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        let action_name = action.and_then(|v| v.as_str()).unwrap_or("");
        match action_name {
            "profile_upsert" => self.profile_upsert(&args).await,
            "profile_get" => self.profile_get(&args),
            "profile_list" => self.profile_list(),
            "profile_delete" => self.profile_delete(&args),
            "profile_test" => self.profile_test(&args).await,
            "query" => self.query(&args).await,
            "batch" => self.batch(&args).await,
            "transaction" => self.transaction(&args).await,
            "insert" => self.insert(&args).await,
            "insert_bulk" => self.insert_bulk(&args).await,
            "update" => self.update(&args).await,
            "delete" => self.delete(&args).await,
            "select" => self.select(&args).await,
            "count" => self.count(&args).await,
            "exists" => self.exists(&args).await,
            "export" => self.export_data(&args).await,
            "catalog_tables" => self.catalog_tables(&args).await,
            "catalog_columns" => self.catalog_columns(&args).await,
            "database_info" => self.database_info(&args).await,
            _ => Err(unknown_action_error("psql", action, PG_ACTIONS)),
        }
    }

    async fn profile_upsert(&self, args: &Value) -> Result<Value, ToolError> {
        let name = self.validation.ensure_string(
            args.get("profile_name").unwrap_or(&Value::Null),
            "profile_name",
            true,
        )?;
        let connection = args
            .get("connection")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let connection_url = args.get("connection_url").and_then(|v| v.as_str());

        let mut merged = merge_connection(connection_url, &connection)?;
        if let Some(pool) = args.get("pool") {
            if let Value::Object(map) = &mut merged {
                map.insert("pool".to_string(), pool.clone());
            }
        }
        if let Some(options) = args.get("options") {
            if let Value::Object(map) = &mut merged {
                map.insert("options".to_string(), options.clone());
            }
        }

        let (data, secrets) = split_connection_secrets(&merged);
        let resolved = if let Some(resolver) = &self.secret_ref_resolver {
            resolver
                .resolve_deep(&Value::Object(data.clone()), args)
                .await?
        } else {
            Value::Object(data.clone())
        };

        let config = build_config_from_value(&resolved, None)?;
        self.test_connection(&config, args.get("pool")).await?;

        let mut profile_payload = serde_json::Map::new();
        profile_payload.insert(
            "type".to_string(),
            Value::String(PG_PROFILE_TYPE.to_string()),
        );
        profile_payload.insert("data".to_string(), Value::Object(data));
        if !secrets.is_empty() {
            profile_payload.insert("secrets".to_string(), Value::Object(secrets));
        }

        let profile = self
            .profile_service
            .set_profile(&name, &Value::Object(profile_payload))?;
        self.pools.remove(&format!("profile:{}", name));

        Ok(serde_json::json!({"success": true, "profile": profile}))
    }

    fn profile_get(&self, args: &Value) -> Result<Value, ToolError> {
        let name = self.validation.ensure_string(
            args.get("profile_name").unwrap_or(&Value::Null),
            "profile_name",
            true,
        )?;
        let profile = self
            .profile_service
            .get_profile(&name, Some(PG_PROFILE_TYPE))?;
        let include_secrets = args
            .get("include_secrets")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let allow = std::env::var("INFRA_ALLOW_SECRET_EXPORT")
            .ok()
            .filter(|v| v.trim() == "1" || v.trim().eq_ignore_ascii_case("true"))
            .is_some();
        if include_secrets && allow {
            return Ok(serde_json::json!({"success": true, "profile": profile}));
        }
        let secret_keys = profile
            .get("secrets")
            .and_then(|v| v.as_object())
            .map(|map| map.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        Ok(serde_json::json!({
            "success": true,
            "profile": {
                "name": profile.get("name").cloned().unwrap_or(Value::String(name)),
                "type": profile.get("type").cloned().unwrap_or(Value::Null),
                "data": profile.get("data").cloned().unwrap_or(Value::Object(Default::default())),
                "secrets": secret_keys,
                "secrets_redacted": true,
            }
        }))
    }

    fn profile_list(&self) -> Result<Value, ToolError> {
        let profiles = self.profile_service.list_profiles(Some(PG_PROFILE_TYPE))?;
        Ok(serde_json::json!({"success": true, "profiles": profiles}))
    }

    fn profile_delete(&self, args: &Value) -> Result<Value, ToolError> {
        let name = self.validation.ensure_string(
            args.get("profile_name").unwrap_or(&Value::Null),
            "profile_name",
            true,
        )?;
        let result = self.profile_service.delete_profile(&name)?;
        self.pools.remove(&format!("profile:{}", name));
        Ok(result)
    }

    async fn profile_test(&self, args: &Value) -> Result<Value, ToolError> {
        let resolved = self.resolve_connection(args).await?;
        self.test_connection(&resolved.config, args.get("pool"))
            .await?;
        Ok(serde_json::json!({"success": true}))
    }

    async fn query(&self, args: &Value) -> Result<Value, ToolError> {
        let sql =
            self.validation
                .ensure_string(args.get("sql").unwrap_or(&Value::Null), "sql", true)?;
        let resolved = self.resolve_connection(args).await?;
        let pool = self.get_pool(&resolved).await?;
        let params = args
            .get("params")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mode = args.get("mode").and_then(|v| v.as_str());
        let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_u64());
        let result = execute_query_with_pool(&pool, &sql, &params, mode, timeout_ms).await?;
        Ok(result)
    }

    async fn batch(&self, args: &Value) -> Result<Value, ToolError> {
        let statements = args
            .get("statements")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if statements.is_empty() {
            return Err(
                ToolError::invalid_params("statements must be a non-empty array")
                    .with_hint("Provide at least one statement: [{ sql: \"SELECT 1\" }]."),
            );
        }
        let resolved = self.resolve_connection(args).await?;
        let pool = self.get_pool(&resolved).await?;
        let transactional = args
            .get("transactional")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut results = Vec::new();

        if !transactional {
            for statement in statements {
                let sql = statement.get("sql").and_then(|v| v.as_str()).unwrap_or("");
                if sql.trim().is_empty() {
                    return Err(ToolError::invalid_params("statement.sql is required"));
                }
                let params = statement
                    .get("params")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let mode = statement.get("mode").and_then(|v| v.as_str());
                let timeout_ms = statement.get("timeout_ms").and_then(|v| v.as_u64());
                let result = execute_query_with_pool(&pool, sql, &params, mode, timeout_ms).await?;
                results.push(result);
            }
            return Ok(serde_json::json!({"success": true, "results": results}));
        }

        let mut conn = pool.get().await.map_err(map_pool_error)?;
        let transaction = conn.transaction().await.map_err(map_pg_error)?;
        for statement in statements {
            let sql = statement.get("sql").and_then(|v| v.as_str()).unwrap_or("");
            if sql.trim().is_empty() {
                return Err(ToolError::invalid_params("statement.sql is required"));
            }
            let params = statement
                .get("params")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mode = statement.get("mode").and_then(|v| v.as_str());
            let timeout_ms = statement.get("timeout_ms").and_then(|v| v.as_u64());
            let result = execute_query(&transaction, sql, &params, mode, timeout_ms).await?;
            results.push(result);
        }
        transaction.commit().await.map_err(map_pg_error)?;
        Ok(serde_json::json!({"success": true, "results": results}))
    }

    async fn transaction(&self, args: &Value) -> Result<Value, ToolError> {
        let mut clone = args.clone();
        if let Value::Object(map) = &mut clone {
            map.insert("transactional".to_string(), Value::Bool(true));
        }
        self.batch(&clone).await
    }

    async fn insert(&self, args: &Value) -> Result<Value, ToolError> {
        let context = normalize_table_context(
            self.validation
                .ensure_string(args.get("table").unwrap_or(&Value::Null), "table", true)?
                .as_str(),
            args.get("schema").and_then(|v| v.as_str()),
        )?;
        let data = self
            .validation
            .ensure_data_object(args.get("data").unwrap_or(&Value::Null))?;
        let columns: Vec<String> = data
            .keys()
            .map(|col| quote_qualified_identifier(col))
            .collect::<Result<Vec<_>, _>>()?;
        let mut values: Vec<Value> = Vec::new();
        for col in data.keys() {
            values.push(data.get(col).cloned().unwrap_or(Value::Null));
        }
        let placeholders: Vec<String> = (1..=values.len()).map(|idx| format!("${}", idx)).collect();
        let returning = build_returning(args.get("returning"));
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}){}",
            context
                .get("qualified")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            columns.join(", "),
            placeholders.join(", "),
            returning
        );
        let resolved = self.resolve_connection(args).await?;
        let pool = self.get_pool(&resolved).await?;
        let result = execute_query_with_pool(
            &pool,
            &sql,
            &values,
            args.get("mode").and_then(|v| v.as_str()),
            args.get("timeout_ms").and_then(|v| v.as_u64()),
        )
        .await?;
        Ok(serde_json::json!({
            "success": true,
            "table": context.get("table").cloned().unwrap_or(Value::Null),
            "schema": context.get("schema").cloned().unwrap_or(Value::Null),
            "result": result,
        }))
    }

    async fn insert_bulk(&self, args: &Value) -> Result<Value, ToolError> {
        let context = normalize_table_context(
            self.validation
                .ensure_string(args.get("table").unwrap_or(&Value::Null), "table", true)?
                .as_str(),
            args.get("schema").and_then(|v| v.as_str()),
        )?;
        let rows = args
            .get("rows")
            .or_else(|| args.get("data"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if rows.is_empty() {
            return Err(ToolError::invalid_params("rows must be a non-empty array")
                .with_hint("Provide args.rows as an array of objects (or arrays) to insert."));
        }
        let mut columns: Option<Vec<String>> =
            args.get("columns").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            });
        if columns.as_ref().map(|c| c.is_empty()).unwrap_or(false) {
            return Err(ToolError::invalid_params(
                "columns must be a non-empty array",
            ));
        }
        if columns.is_none() {
            let first = rows[0].as_object().ok_or_else(|| {
                ToolError::invalid_params("rows must be objects or provide columns")
            })?;
            let mut cols = Vec::new();
            for key in first.keys() {
                cols.push(key.clone());
            }
            columns = Some(cols);
        }
        let columns = columns.unwrap();
        let column_sql = columns
            .iter()
            .map(|col| quote_qualified_identifier(col))
            .collect::<Result<Vec<_>, _>>()?;
        let returning = build_returning(args.get("returning"));

        let max_params = 65535usize;
        let max_batch = std::cmp::max(1, max_params / column_sql.len());
        let requested_batch = args
            .get("batch_size")
            .and_then(|v| v.as_u64())
            .unwrap_or(500) as usize;
        let batch_size = std::cmp::min(requested_batch, max_batch);

        let resolved = self.resolve_connection(args).await?;
        let pool = self.get_pool(&resolved).await?;

        let mut inserted = 0usize;
        let mut all_rows: Vec<Value> = Vec::new();

        for offset in (0..rows.len()).step_by(batch_size) {
            let batch = &rows[offset..std::cmp::min(offset + batch_size, rows.len())];
            let mut values: Vec<Value> = Vec::new();
            let mut placeholders = Vec::new();
            for (row_index, row) in batch.iter().enumerate() {
                let row_values = if let Some(obj) = row.as_object() {
                    columns
                        .iter()
                        .map(|col| obj.get(col).cloned().unwrap_or(Value::Null))
                        .collect::<Vec<_>>()
                } else if let Some(arr) = row.as_array() {
                    columns
                        .iter()
                        .enumerate()
                        .map(|(idx, _)| arr.get(idx).cloned().unwrap_or(Value::Null))
                        .collect::<Vec<_>>()
                } else {
                    return Err(ToolError::invalid_params(
                        "Each row must be an object or array",
                    ));
                };
                let start_index = row_index * column_sql.len();
                let row_placeholders = (0..column_sql.len())
                    .map(|col_idx| format!("${}", start_index + col_idx + 1))
                    .collect::<Vec<_>>();
                placeholders.push(format!("({})", row_placeholders.join(", ")));
                values.extend(row_values);
            }
            let sql = format!(
                "INSERT INTO {} ({}) VALUES {}{}",
                context
                    .get("qualified")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                column_sql.join(", "),
                placeholders.join(", "),
                returning
            );
            let result = execute_query_with_pool(
                &pool,
                &sql,
                &values,
                args.get("mode").and_then(|v| v.as_str()),
                args.get("timeout_ms").and_then(|v| v.as_u64()),
            )
            .await?;
            inserted += batch.len();
            if let Some(rows) = result.get("rows").and_then(|v| v.as_array()) {
                all_rows.extend(rows.iter().cloned());
            }
        }

        Ok(serde_json::json!({
            "success": true,
            "table": context.get("table").cloned().unwrap_or(Value::Null),
            "schema": context.get("schema").cloned().unwrap_or(Value::Null),
            "inserted": inserted,
            "batches": rows.len().div_ceil(batch_size),
            "rows": if returning.is_empty() { Value::Null } else { Value::Array(all_rows) },
        }))
    }

    async fn update(&self, args: &Value) -> Result<Value, ToolError> {
        let context = normalize_table_context(
            self.validation
                .ensure_string(args.get("table").unwrap_or(&Value::Null), "table", true)?
                .as_str(),
            args.get("schema").and_then(|v| v.as_str()),
        )?;
        let data = self
            .validation
            .ensure_data_object(args.get("data").unwrap_or(&Value::Null))?;
        let columns: Vec<String> = data
            .keys()
            .map(|col| quote_qualified_identifier(col))
            .collect::<Result<Vec<_>, _>>()?;
        let mut values: Vec<Value> = Vec::new();
        for col in data.keys() {
            values.push(data.get(col).cloned().unwrap_or(Value::Null));
        }
        let assignments: Vec<String> = columns
            .iter()
            .enumerate()
            .map(|(idx, col)| format!("{} = ${}", col, idx + 1))
            .collect();
        let (where_sql, where_params, _) = build_where_clause(
            args.get("filters"),
            args.get("where_sql").and_then(|v| v.as_str()),
            args.get("where_params").and_then(|v| v.as_array()),
            values.len() as i64 + 1,
        )?;
        let returning = build_returning(args.get("returning"));
        let sql = format!(
            "UPDATE {} SET {}{}{}",
            context
                .get("qualified")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            assignments.join(", "),
            if where_sql.is_empty() { "" } else { " WHERE " },
            if where_sql.is_empty() {
                ""
            } else {
                where_sql.as_str()
            },
        );
        let sql = format!("{}{}", sql, returning);
        let mut params = values;
        params.extend(where_params);
        let resolved = self.resolve_connection(args).await?;
        let pool = self.get_pool(&resolved).await?;
        let result = execute_query_with_pool(
            &pool,
            &sql,
            &params,
            args.get("mode").and_then(|v| v.as_str()),
            args.get("timeout_ms").and_then(|v| v.as_u64()),
        )
        .await?;
        Ok(serde_json::json!({
            "success": true,
            "table": context.get("table").cloned().unwrap_or(Value::Null),
            "schema": context.get("schema").cloned().unwrap_or(Value::Null),
            "result": result,
        }))
    }

    async fn delete(&self, args: &Value) -> Result<Value, ToolError> {
        let context = normalize_table_context(
            self.validation
                .ensure_string(args.get("table").unwrap_or(&Value::Null), "table", true)?
                .as_str(),
            args.get("schema").and_then(|v| v.as_str()),
        )?;
        let (where_sql, where_params, _) = build_where_clause(
            args.get("filters"),
            args.get("where_sql").and_then(|v| v.as_str()),
            args.get("where_params").and_then(|v| v.as_array()),
            1,
        )?;
        let returning = build_returning(args.get("returning"));
        let sql = format!(
            "DELETE FROM {}{}{}",
            context
                .get("qualified")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            if where_sql.is_empty() { "" } else { " WHERE " },
            if where_sql.is_empty() {
                ""
            } else {
                where_sql.as_str()
            },
        );
        let sql = format!("{}{}", sql, returning);
        let resolved = self.resolve_connection(args).await?;
        let pool = self.get_pool(&resolved).await?;
        let result = execute_query_with_pool(
            &pool,
            &sql,
            &where_params,
            args.get("mode").and_then(|v| v.as_str()),
            args.get("timeout_ms").and_then(|v| v.as_u64()),
        )
        .await?;
        Ok(serde_json::json!({
            "success": true,
            "table": context.get("table").cloned().unwrap_or(Value::Null),
            "schema": context.get("schema").cloned().unwrap_or(Value::Null),
            "result": result,
        }))
    }

    async fn select(&self, args: &Value) -> Result<Value, ToolError> {
        let (sql, params, context) = build_select_query(args, "select")?;
        let resolved = self.resolve_connection(args).await?;
        let pool = self.get_pool(&resolved).await?;
        let result = execute_query_with_pool(
            &pool,
            &sql,
            &params,
            args.get("mode").and_then(|v| v.as_str()),
            args.get("timeout_ms").and_then(|v| v.as_u64()),
        )
        .await?;
        Ok(serde_json::json!({
            "success": true,
            "table": context.get("table").cloned().unwrap_or(Value::Null),
            "schema": context.get("schema").cloned().unwrap_or(Value::Null),
            "result": result,
        }))
    }

    async fn count(&self, args: &Value) -> Result<Value, ToolError> {
        let context = normalize_table_context(
            self.validation
                .ensure_string(args.get("table").unwrap_or(&Value::Null), "table", true)?
                .as_str(),
            args.get("schema").and_then(|v| v.as_str()),
        )?;
        let (where_sql, where_params, _) = build_where_clause(
            args.get("filters"),
            args.get("where_sql").and_then(|v| v.as_str()),
            args.get("where_params").and_then(|v| v.as_array()),
            1,
        )?;
        let sql = format!(
            "SELECT COUNT(*) AS count FROM {}{}{}",
            context
                .get("qualified")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            if where_sql.is_empty() { "" } else { " WHERE " },
            if where_sql.is_empty() {
                ""
            } else {
                where_sql.as_str()
            },
        );
        let resolved = self.resolve_connection(args).await?;
        let pool = self.get_pool(&resolved).await?;
        let result = execute_query_with_pool(
            &pool,
            &sql,
            &where_params,
            Some("row"),
            args.get("timeout_ms").and_then(|v| v.as_u64()),
        )
        .await?;
        let count = result
            .get("row")
            .and_then(|v| v.get("count"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        Ok(serde_json::json!({
            "success": true,
            "table": context.get("table").cloned().unwrap_or(Value::Null),
            "schema": context.get("schema").cloned().unwrap_or(Value::Null),
            "count": count,
        }))
    }

    async fn exists(&self, args: &Value) -> Result<Value, ToolError> {
        let context = normalize_table_context(
            self.validation
                .ensure_string(args.get("table").unwrap_or(&Value::Null), "table", true)?
                .as_str(),
            args.get("schema").and_then(|v| v.as_str()),
        )?;
        let (where_sql, where_params, _) = build_where_clause(
            args.get("filters"),
            args.get("where_sql").and_then(|v| v.as_str()),
            args.get("where_params").and_then(|v| v.as_array()),
            1,
        )?;
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM {}{}{}) AS exists",
            context
                .get("qualified")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            if where_sql.is_empty() { "" } else { " WHERE " },
            if where_sql.is_empty() {
                ""
            } else {
                where_sql.as_str()
            },
        );
        let resolved = self.resolve_connection(args).await?;
        let pool = self.get_pool(&resolved).await?;
        let result = execute_query_with_pool(
            &pool,
            &sql,
            &where_params,
            Some("row"),
            args.get("timeout_ms").and_then(|v| v.as_u64()),
        )
        .await?;
        let exists = result
            .get("row")
            .and_then(|v| v.get("exists"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(serde_json::json!({
            "success": true,
            "table": context.get("table").cloned().unwrap_or(Value::Null),
            "schema": context.get("schema").cloned().unwrap_or(Value::Null),
            "exists": exists,
        }))
    }

    async fn export_data(&self, args: &Value) -> Result<Value, ToolError> {
        let file_path = args.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
        if file_path.trim().is_empty() {
            return Err(ToolError::invalid_params("file_path is required"));
        }
        let overwrite = args
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let path = crate::utils::user_paths::expand_home_path(file_path);
        if path.exists() && !overwrite {
            return Err(ToolError::conflict(format!(
                "Local path already exists: {}",
                path.display()
            ))
            .with_hint("Set overwrite=true to replace it."));
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let tmp_path = path.with_extension("part");
        let mut file = tokio::fs::File::create(&tmp_path)
            .await
            .map_err(|err| ToolError::internal(format!("Failed to create export file: {}", err)))?;
        let result = self.export_to_writer(args, &mut file).await;
        if let Err(err) = result {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(err);
        }
        file.flush().await.ok();
        drop(file);
        tokio::fs::rename(&tmp_path, &path).await.map_err(|err| {
            ToolError::internal(format!("Failed to finalize export file: {}", err))
        })?;
        let mut out = result.unwrap();
        if let Value::Object(map) = &mut out {
            map.insert(
                "file_path".to_string(),
                Value::String(path.display().to_string()),
            );
        }
        Ok(out)
    }

    async fn export_to_writer<W>(&self, args: &Value, writer: &mut W) -> Result<Value, ToolError>
    where
        W: AsyncWrite + Unpin,
    {
        let format = args
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("csv")
            .to_lowercase();
        if format != "csv" && format != "jsonl" {
            return Err(ToolError::invalid_params("format must be csv or jsonl"));
        }
        let batch_size = args
            .get("batch_size")
            .and_then(|v| v.as_u64())
            .unwrap_or(1000) as usize;
        let base_offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let header_enabled = args
            .get("csv_header")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let delimiter = args
            .get("csv_delimiter")
            .and_then(|v| v.as_str())
            .unwrap_or(",")
            .to_string();

        let (sql, params, context) = build_select_query(args, "export")?;
        let resolved = self.resolve_connection(args).await?;
        let pool = self.get_pool(&resolved).await?;

        let mut offset = base_offset;
        let mut rows_written = 0usize;
        let mut header_written = false;
        let mut columns: Option<Vec<String>> = None;

        loop {
            let page_limit = match limit {
                Some(limit) => {
                    if rows_written >= limit {
                        0
                    } else {
                        std::cmp::min(batch_size, limit - rows_written)
                    }
                }
                None => batch_size,
            };
            if page_limit == 0 {
                break;
            }

            let paged_sql = format!("{} LIMIT {} OFFSET {}", sql, page_limit, offset);
            let result = execute_query_with_pool(
                &pool,
                &paged_sql,
                &params,
                Some("rows"),
                args.get("timeout_ms").and_then(|v| v.as_u64()),
            )
            .await?;
            let rows = result
                .get("rows")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if rows.is_empty() {
                break;
            }

            if format == "csv" && header_enabled && !header_written {
                if let Some(first) = rows.first().and_then(|v| v.as_object()) {
                    columns = Some(first.keys().cloned().collect());
                }
                if let Some(cols) = columns.as_ref() {
                    let line = cols
                        .iter()
                        .map(|c| csv_escape(c, &delimiter))
                        .collect::<Vec<_>>()
                        .join(&delimiter);
                    writer
                        .write_all(format!("{}\n", line).as_bytes())
                        .await
                        .ok();
                }
                header_written = true;
            }

            for row in rows {
                if format == "jsonl" {
                    writer.write_all(format!("{}\n", row).as_bytes()).await.ok();
                } else {
                    let cols = columns.clone().unwrap_or_else(|| {
                        row.as_object()
                            .map(|m| m.keys().cloned().collect())
                            .unwrap_or_default()
                    });
                    let line = cols
                        .iter()
                        .map(|col| {
                            let value = row.get(col.as_str()).cloned().unwrap_or(Value::Null);
                            csv_escape(&value.to_string(), &delimiter)
                        })
                        .collect::<Vec<_>>()
                        .join(&delimiter);
                    writer
                        .write_all(format!("{}\n", line).as_bytes())
                        .await
                        .ok();
                }
                rows_written += 1;
                if let Some(limit) = limit {
                    if rows_written >= limit {
                        break;
                    }
                }
            }

            if let Some(limit) = limit {
                if rows_written >= limit {
                    break;
                }
            }
            offset += page_limit;
        }

        Ok(serde_json::json!({
            "success": true,
            "table": context.get("table").cloned().unwrap_or(Value::Null),
            "schema": context.get("schema").cloned().unwrap_or(Value::Null),
            "format": format,
            "rows_written": rows_written,
        }))
    }

    pub(crate) fn export_stream(&self, args: &Value) -> ExportStream {
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        let args = args.clone();
        let manager = self.clone();
        let completion = tokio::spawn(async move {
            let result = manager.export_to_writer(&args, &mut writer).await;
            let _ = writer.shutdown().await;
            result
        });
        ExportStream { reader, completion }
    }

    async fn catalog_tables(&self, args: &Value) -> Result<Value, ToolError> {
        let schema = args.get("schema").and_then(|v| v.as_str());
        let sql = format!(
            "SELECT schemaname AS schema, tablename AS name, tableowner AS owner, hasindexes, hasrules, hastriggers FROM pg_tables WHERE schemaname NOT IN ('pg_catalog', 'information_schema') {} ORDER BY schemaname, tablename",
            if schema.is_some() { "AND schemaname = $1" } else { "" }
        );
        let params = schema
            .map(|s| vec![Value::String(s.to_string())])
            .unwrap_or_default();
        self.query_with_params(args, &sql, &params).await
    }

    async fn catalog_columns(&self, args: &Value) -> Result<Value, ToolError> {
        let table = self.validation.ensure_string(
            args.get("table").unwrap_or(&Value::Null),
            "table",
            true,
        )?;
        let schema = args
            .get("schema")
            .and_then(|v| v.as_str())
            .unwrap_or("public");
        let sql = "SELECT column_name, data_type, is_nullable, column_default, character_maximum_length, numeric_precision, numeric_scale FROM information_schema.columns WHERE table_schema = $1 AND table_name = $2 ORDER BY ordinal_position";
        let params = vec![
            Value::String(schema.to_string()),
            Value::String(table.clone()),
        ];
        let result = self.query_with_params(args, sql, &params).await?;
        Ok(
            serde_json::json!({"success": true, "table": table, "schema": schema, "columns": result.get("rows").cloned().unwrap_or(Value::Null)}),
        )
    }

    async fn database_info(&self, args: &Value) -> Result<Value, ToolError> {
        let sql = "SELECT current_database() AS database_name, current_user AS current_user, version() AS version, pg_size_pretty(pg_database_size(current_database())) AS size";
        self.query_with_params(args, sql, &[]).await
    }

    async fn query_with_params(
        &self,
        args: &Value,
        sql: &str,
        params: &[Value],
    ) -> Result<Value, ToolError> {
        let resolved = self.resolve_connection(args).await?;
        let pool = self.get_pool(&resolved).await?;
        execute_query_with_pool(
            &pool,
            sql,
            params,
            Some("rows"),
            args.get("timeout_ms").and_then(|v| v.as_u64()),
        )
        .await
    }

    async fn resolve_connection(&self, args: &Value) -> Result<ResolvedConnection, ToolError> {
        let inline_connection =
            args.get("connection").is_some() || args.get("connection_url").is_some();
        if !inline_connection {
            let profile_name = self.resolve_profile_name(args).await?;
            let Some(profile_name) = profile_name else {
                return Err(
                    ToolError::invalid_params("profile_name or connection is required").with_hint(
                        "Pass args.profile_name, or provide args.connection / args.connection_url.",
                    ),
                );
            };
            let profile = self
                .profile_service
                .get_profile(&profile_name, Some(PG_PROFILE_TYPE))?;
            let mut merged = profile
                .get("data")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            if let Some(secrets) = profile.get("secrets") {
                if let Value::Object(map) = &mut merged {
                    for (key, value) in secrets.as_object().cloned().unwrap_or_default() {
                        map.insert(key, value);
                    }
                }
            }
            let resolved = if let Some(resolver) = &self.secret_ref_resolver {
                resolver.resolve_deep(&merged, args).await?
            } else {
                merged.clone()
            };
            let pool_options = pool_options_from_value(resolved.get("pool"));
            let config = build_config_from_value(&resolved, None)?;
            let key_seed = format!("profile:{}", profile_name);
            return Ok(ResolvedConnection {
                config,
                pool_options,
                profile_name: Some(profile_name),
                key_seed,
            });
        }

        let connection = args
            .get("connection")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let connection_url = args.get("connection_url").and_then(|v| v.as_str());
        let merged = merge_connection(connection_url, &connection)?;
        let resolved = if let Some(resolver) = &self.secret_ref_resolver {
            resolver.resolve_deep(&merged, args).await?
        } else {
            merged.clone()
        };
        let pool_options = pool_options_from_value(resolved.get("pool"))
            .merge(pool_options_from_value(args.get("pool")));
        let config = build_config_from_value(&resolved, connection_url)?;
        let key_seed = format!("inline:{}", hash_seed(&resolved, &pool_options));
        Ok(ResolvedConnection {
            config,
            pool_options,
            profile_name: None,
            key_seed,
        })
    }

    async fn resolve_profile_name(&self, args: &Value) -> Result<Option<String>, ToolError> {
        if let Some(name) = args.get("profile_name").and_then(|v| v.as_str()) {
            return Ok(Some(
                self.validation.ensure_identifier(name, "profile_name")?,
            ));
        }
        if let Some(resolver) = &self.project_resolver {
            if let Ok(context) = resolver.resolve_context(args).await {
                if let Some(profile) = context
                    .as_ref()
                    .and_then(|v| v.get("target"))
                    .and_then(|v| v.get("postgres_profile"))
                    .and_then(|v| v.as_str())
                {
                    return Ok(Some(
                        self.validation.ensure_identifier(profile, "profile_name")?,
                    ));
                }
            }
        }
        let profiles = self.profile_service.list_profiles(Some(PG_PROFILE_TYPE))?;
        if let Some(arr) = profiles.as_array() {
            if arr.len() == 1 {
                if let Some(name) = arr[0].get("name").and_then(|v| v.as_str()) {
                    return Ok(Some(name.to_string()));
                }
            }
            if arr.is_empty() {
                return Ok(None);
            }
            return Err(ToolError::invalid_params("profile_name is required when multiple profiles exist")
                .with_details(serde_json::json!({"known_profiles": arr.iter().filter_map(|v| v.get("name")).collect::<Vec<_>>() })));
        }
        Ok(None)
    }

    async fn get_pool(
        &self,
        resolved: &ResolvedConnection,
    ) -> Result<Pool<PostgresConnectionManager<NoTls>>, ToolError> {
        if let Some(profile_name) = resolved.profile_name.as_ref() {
            let key = format!("profile:{}", profile_name);
            if let Some(existing) = self.pools.get(&key) {
                return Ok(existing.value().clone());
            }
        }
        if let Some(existing) = self.pools.get(&resolved.key_seed) {
            return Ok(existing.value().clone());
        }

        let manager = PostgresConnectionManager::new(resolved.config.clone(), NoTls);
        let mut builder = Pool::builder();
        if let Some(max) = resolved.pool_options.max_size {
            builder = builder.max_size(max);
        } else {
            builder = builder.max_size(limit_constants::MAX_CONNECTIONS as u32);
        }
        if let Some(min) = resolved.pool_options.min_idle {
            builder = builder.min_idle(Some(min));
        }
        if let Some(timeout) = resolved.pool_options.idle_timeout_ms {
            builder = builder.idle_timeout(Some(Duration::from_millis(timeout)));
        }
        if let Some(timeout) = resolved.pool_options.connection_timeout_ms {
            builder = builder.connection_timeout(Duration::from_millis(timeout));
        } else {
            builder = builder.connection_timeout(Duration::from_millis(
                network_constants::TIMEOUT_CONNECTION_MS,
            ));
        }

        let pool = builder.build(manager).await.map_err(map_pool_error)?;
        let key = resolved.key_seed.clone();
        self.pools.insert(key.clone(), pool.clone());
        if let Some(profile_name) = resolved.profile_name.as_ref() {
            self.pools
                .insert(format!("profile:{}", profile_name), pool.clone());
        }
        Ok(pool)
    }

    async fn test_connection(
        &self,
        config: &Config,
        pool_value: Option<&Value>,
    ) -> Result<(), ToolError> {
        let manager = PostgresConnectionManager::new(config.clone(), NoTls);
        let mut builder = Pool::builder();
        if let Some(pool_opts) = pool_value {
            let opts = pool_options_from_value(Some(pool_opts));
            if let Some(max) = opts.max_size {
                builder = builder.max_size(max);
            }
        }
        let pool = builder.build(manager).await.map_err(map_pool_error)?;
        let conn = pool.get().await.map_err(map_pool_error)?;
        conn.query("SELECT 1", &[]).await.map_err(map_pg_error)?;
        drop(conn);
        Ok(())
    }
}

fn build_select_query(args: &Value, mode: &str) -> Result<(String, Vec<Value>, Value), ToolError> {
    let table = args.get("table").and_then(|v| v.as_str()).unwrap_or("");
    let schema = args.get("schema").and_then(|v| v.as_str());
    let context = normalize_table_context(table, schema)?;
    let columns_sql = normalize_columns(args.get("columns"), args.get("columns_sql"))?;
    let (where_sql, params, _) = build_where_clause(
        args.get("filters"),
        args.get("where_sql").and_then(|v| v.as_str()),
        args.get("where_params").and_then(|v| v.as_array()),
        1,
    )?;
    let order_by_sql = if mode == "select" {
        build_order_by(args.get("order_by"), args.get("order_by_sql"))?
    } else {
        String::new()
    };
    let limit = if mode == "select" {
        normalize_limit(args.get("limit"), "limit")?
    } else {
        None
    };
    let offset = if mode == "select" {
        normalize_limit(args.get("offset"), "offset")?
    } else {
        None
    };

    let sql = format!(
        "SELECT {} FROM {}{}{}{}{}",
        columns_sql,
        context
            .get("qualified")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        if where_sql.is_empty() { "" } else { " WHERE " },
        if where_sql.is_empty() {
            ""
        } else {
            where_sql.as_str()
        },
        order_by_sql,
        match limit {
            Some(limit) => format!(" LIMIT {}", limit),
            None => String::new(),
        },
    );
    let sql = if let Some(offset) = offset {
        format!("{} OFFSET {}", sql, offset)
    } else {
        sql
    };

    Ok((sql, params, context))
}

fn normalize_columns(
    columns: Option<&Value>,
    columns_sql: Option<&Value>,
) -> Result<String, ToolError> {
    if let Some(sql) = columns_sql.and_then(|v| v.as_str()) {
        return Ok(sql.to_string());
    }
    match columns {
        None => Ok("*".to_string()),
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Err(ToolError::invalid_params(
                    "columns must be a non-empty string",
                ));
            }
            Ok(trimmed.to_string())
        }
        Some(Value::Array(arr)) => {
            if arr.is_empty() {
                return Err(ToolError::invalid_params(
                    "columns must be a non-empty array",
                ));
            }
            let mut out = Vec::new();
            for col in arr {
                let name = col
                    .as_str()
                    .ok_or_else(|| ToolError::invalid_params("columns must be strings"))?;
                out.push(quote_qualified_identifier(name)?);
            }
            Ok(out.join(", "))
        }
        _ => Err(ToolError::invalid_params(
            "columns must be a string or array",
        )),
    }
}

fn normalize_limit(value: Option<&Value>, label: &str) -> Result<Option<usize>, ToolError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let numeric = value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.parse::<i64>().ok()))
        .ok_or_else(|| {
            ToolError::invalid_params(format!("{} must be a non-negative integer", label))
        })?;
    if numeric < 0 {
        return Err(ToolError::invalid_params(format!(
            "{} must be a non-negative integer",
            label
        )));
    }
    Ok(Some(numeric as usize))
}

fn build_order_by(
    order_by: Option<&Value>,
    order_by_sql: Option<&Value>,
) -> Result<String, ToolError> {
    if let Some(sql) = order_by_sql.and_then(|v| v.as_str()) {
        if sql.trim().is_empty() {
            return Ok(String::new());
        }
        return Ok(format!(" ORDER BY {}", sql));
    }
    let Some(order_by) = order_by else {
        return Ok(String::new());
    };
    let mut parts = Vec::new();
    match order_by {
        Value::String(text) => {
            let col = text.trim();
            if !col.is_empty() {
                parts.push(format!("{} ASC", quote_qualified_identifier(col)?));
            }
        }
        Value::Array(arr) => {
            for entry in arr {
                if let Some(text) = entry.as_str() {
                    parts.push(format!("{} ASC", quote_qualified_identifier(text)?));
                    continue;
                }
                if let Some(obj) = entry.as_object() {
                    let column = obj
                        .get("column")
                        .or_else(|| obj.get("field"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if column.is_empty() {
                        continue;
                    }
                    let direction = obj
                        .get("direction")
                        .and_then(|v| v.as_str())
                        .unwrap_or("asc");
                    let dir = if direction.to_lowercase() == "desc" {
                        "DESC"
                    } else {
                        "ASC"
                    };
                    parts.push(format!("{} {}", quote_qualified_identifier(column)?, dir));
                }
            }
        }
        Value::Object(map) => {
            for (column, direction) in map {
                let dir = direction.as_str().unwrap_or("asc");
                let dir = if dir.to_lowercase() == "desc" {
                    "DESC"
                } else {
                    "ASC"
                };
                parts.push(format!("{} {}", quote_qualified_identifier(column)?, dir));
            }
        }
        _ => {}
    }
    if parts.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(" ORDER BY {}", parts.join(", ")))
    }
}

fn build_returning(returning: Option<&Value>) -> String {
    let Some(value) = returning else {
        return String::new();
    };
    if value.is_boolean() {
        if value.as_bool().unwrap_or(false) {
            return " RETURNING *".to_string();
        }
        return String::new();
    }
    if let Some(text) = value.as_str() {
        if text.trim().is_empty() {
            return String::new();
        }
        return format!(
            " RETURNING {}",
            quote_qualified_identifier(text).unwrap_or_else(|_| text.to_string())
        );
    }
    if let Some(arr) = value.as_array() {
        if arr.is_empty() {
            return String::new();
        }
        let mut cols = Vec::new();
        for col in arr {
            if let Some(text) = col.as_str() {
                cols.push(quote_qualified_identifier(text).unwrap_or_else(|_| text.to_string()));
            }
        }
        if cols.is_empty() {
            return String::new();
        }
        return format!(" RETURNING {}", cols.join(", "));
    }
    String::new()
}

fn csv_escape(value: &str, delimiter: &str) -> String {
    if value.contains('"') || value.contains(delimiter) || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn merge_connection(connection_url: Option<&str>, connection: &Value) -> Result<Value, ToolError> {
    let mut map = serde_json::Map::new();
    if let Some(obj) = connection.as_object() {
        for (k, v) in obj {
            map.insert(k.clone(), v.clone());
        }
    }
    if let Some(url) = connection_url {
        map.insert("connection_url".to_string(), Value::String(url.to_string()));
    }
    Ok(Value::Object(map))
}

fn split_connection_secrets(
    connection: &Value,
) -> (
    serde_json::Map<String, Value>,
    serde_json::Map<String, Value>,
) {
    let mut data = serde_json::Map::new();
    let mut secrets = serde_json::Map::new();
    if let Some(obj) = connection.as_object() {
        for (key, value) in obj {
            if key == "password" || key.starts_with("ssl_") {
                secrets.insert(key.clone(), value.clone());
            } else {
                data.insert(key.clone(), value.clone());
            }
        }
    }
    (data, secrets)
}

fn build_config_from_value(
    connection: &Value,
    connection_url: Option<&str>,
) -> Result<Config, ToolError> {
    let mut config = if let Some(url) =
        connection_url.or_else(|| connection.get("connection_url").and_then(|v| v.as_str()))
    {
        Config::from_str(url).map_err(|_| ToolError::invalid_params("Invalid connection_url"))?
    } else {
        Config::new()
    };
    if let Some(host) = connection.get("host").and_then(|v| v.as_str()) {
        config.host(host);
    }
    if let Some(port) = connection.get("port").and_then(|v| v.as_u64()) {
        config.port(port as u16);
    }
    if let Some(user) = connection
        .get("user")
        .or_else(|| connection.get("username"))
        .and_then(|v| v.as_str())
    {
        config.user(user);
    }
    if let Some(db) = connection
        .get("database")
        .or_else(|| connection.get("dbname"))
        .and_then(|v| v.as_str())
    {
        config.dbname(db);
    }
    if let Some(password) = connection.get("password").and_then(|v| v.as_str()) {
        config.password(password);
    }
    if let Some(options) = connection.get("options").and_then(|v| v.as_str()) {
        config.options(options);
    }
    Ok(config)
}

fn pool_options_from_value(value: Option<&Value>) -> PoolOptions {
    let mut opts = PoolOptions::default();
    let Some(Value::Object(map)) = value else {
        return opts;
    };
    opts.max_size = map
        .get("max")
        .or_else(|| map.get("max_size"))
        .or_else(|| map.get("max_connections"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    opts.min_idle = map
        .get("min_idle")
        .or_else(|| map.get("min"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    opts.idle_timeout_ms = map
        .get("idle_timeout_ms")
        .or_else(|| map.get("idleTimeoutMillis"))
        .and_then(|v| v.as_u64());
    opts.connection_timeout_ms = map
        .get("connection_timeout_ms")
        .or_else(|| map.get("connectionTimeoutMillis"))
        .and_then(|v| v.as_u64());
    opts
}

impl PoolOptions {
    fn merge(mut self, other: PoolOptions) -> PoolOptions {
        if other.max_size.is_some() {
            self.max_size = other.max_size;
        }
        if other.min_idle.is_some() {
            self.min_idle = other.min_idle;
        }
        if other.idle_timeout_ms.is_some() {
            self.idle_timeout_ms = other.idle_timeout_ms;
        }
        if other.connection_timeout_ms.is_some() {
            self.connection_timeout_ms = other.connection_timeout_ms;
        }
        self
    }
}

fn hash_seed(connection: &Value, pool: &PoolOptions) -> String {
    let payload = serde_json::json!({"connection": connection, "pool": {
        "max_size": pool.max_size,
        "min_idle": pool.min_idle,
        "idle_timeout_ms": pool.idle_timeout_ms,
        "connection_timeout_ms": pool.connection_timeout_ms,
    }});
    let mut hasher = Sha256::new();
    hasher.update(payload.to_string().as_bytes());
    hex::encode(hasher.finalize())
}

async fn execute_query<C: GenericClient + Sync>(
    client: &C,
    sql: &str,
    params: &[Value],
    mode: Option<&str>,
    timeout_ms: Option<u64>,
) -> Result<Value, ToolError> {
    let bindings = build_params(params);
    let bind_refs: Vec<&(dyn ToSql + Sync)> = bindings
        .iter()
        .map(|b| b.as_ref() as &(dyn ToSql + Sync))
        .collect();
    let started = std::time::Instant::now();
    let query_fut = client.query(sql, &bind_refs);
    let rows = if let Some(timeout_ms) = timeout_ms {
        tokio::time::timeout(Duration::from_millis(timeout_ms), query_fut)
            .await
            .map_err(|_| ToolError::timeout("PostgreSQL query timed out"))?
            .map_err(map_pg_error)?
    } else {
        query_fut.await.map_err(map_pg_error)?
    };

    let duration_ms = started.elapsed().as_millis();
    let row_count = rows.len() as i64;
    let command = sql.split_whitespace().next().unwrap_or("").to_uppercase();
    let fields = rows
        .first()
        .map(|row| {
            row.columns()
                .iter()
                .map(|col| serde_json::json!({"name": col.name(), "dataTypeId": col.type_().oid()}))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let normalized_mode = mode.unwrap_or("rows").to_lowercase();
    let mut payload = serde_json::json!({
        "success": true,
        "command": command,
        "rowCount": row_count,
        "fields": fields,
        "duration_ms": duration_ms,
    });

    if normalized_mode == "row" {
        let row = rows.first().map(row_to_value).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut payload {
            map.insert("row".to_string(), row);
        }
        return Ok(payload);
    }
    if normalized_mode == "value" {
        let value = rows
            .first()
            .and_then(|row| {
                row_to_value(row)
                    .as_object()
                    .and_then(|m| m.values().next().cloned())
            })
            .unwrap_or(Value::Null);
        if let Value::Object(map) = &mut payload {
            map.insert("value".to_string(), value);
        }
        return Ok(payload);
    }
    if normalized_mode == "command" {
        return Ok(payload);
    }

    let rows_json = rows.iter().map(row_to_value).collect::<Vec<_>>();
    if let Value::Object(map) = &mut payload {
        map.insert("rows".to_string(), Value::Array(rows_json));
    }
    Ok(payload)
}

async fn execute_query_with_pool(
    pool: &Pool<PostgresConnectionManager<NoTls>>,
    sql: &str,
    params: &[Value],
    mode: Option<&str>,
    timeout_ms: Option<u64>,
) -> Result<Value, ToolError> {
    let conn = pool.get().await.map_err(map_pool_error)?;
    let client = &*conn;
    execute_query(client, sql, params, mode, timeout_ms).await
}

fn build_params(values: &[Value]) -> Vec<Box<dyn ToSql + Sync + Send>> {
    values
        .iter()
        .map(|value| match value {
            Value::Null => Box::new(Option::<String>::None) as Box<dyn ToSql + Sync + Send>,
            Value::Bool(val) => Box::new(*val) as Box<dyn ToSql + Sync + Send>,
            Value::Number(num) => {
                if let Some(int) = num.as_i64() {
                    Box::new(int) as Box<dyn ToSql + Sync + Send>
                } else if let Some(float) = num.as_f64() {
                    Box::new(float) as Box<dyn ToSql + Sync + Send>
                } else {
                    Box::new(num.to_string()) as Box<dyn ToSql + Sync + Send>
                }
            }
            Value::String(text) => Box::new(text.clone()) as Box<dyn ToSql + Sync + Send>,
            Value::Array(_) | Value::Object(_) => {
                Box::new(Json(value.clone())) as Box<dyn ToSql + Sync + Send>
            }
        })
        .collect()
}

fn row_to_value(row: &Row) -> Value {
    let mut map = serde_json::Map::new();
    for (idx, col) in row.columns().iter().enumerate() {
        let value = match *col.type_() {
            Type::BOOL => row
                .try_get::<usize, Option<bool>>(idx)
                .ok()
                .flatten()
                .map(Value::Bool)
                .unwrap_or(Value::Null),
            Type::INT2 | Type::INT4 | Type::INT8 => row
                .try_get::<usize, Option<i64>>(idx)
                .ok()
                .flatten()
                .map(|v| Value::Number(v.into()))
                .unwrap_or(Value::Null),
            Type::FLOAT4 | Type::FLOAT8 => row
                .try_get::<usize, Option<f64>>(idx)
                .ok()
                .flatten()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            Type::JSON | Type::JSONB => row
                .try_get::<usize, Option<Value>>(idx)
                .ok()
                .flatten()
                .unwrap_or(Value::Null),
            Type::UUID => row
                .try_get::<usize, Option<uuid::Uuid>>(idx)
                .ok()
                .flatten()
                .map(|v| Value::String(v.to_string()))
                .unwrap_or(Value::Null),
            Type::TIMESTAMP => row
                .try_get::<usize, Option<chrono::NaiveDateTime>>(idx)
                .ok()
                .flatten()
                .map(|v| Value::String(v.to_string()))
                .unwrap_or(Value::Null),
            Type::TIMESTAMPTZ => row
                .try_get::<usize, Option<chrono::DateTime<chrono::Utc>>>(idx)
                .ok()
                .flatten()
                .map(|v| Value::String(v.to_rfc3339()))
                .unwrap_or(Value::Null),
            Type::DATE => row
                .try_get::<usize, Option<chrono::NaiveDate>>(idx)
                .ok()
                .flatten()
                .map(|v| Value::String(v.to_string()))
                .unwrap_or(Value::Null),
            _ => row
                .try_get::<usize, Option<String>>(idx)
                .ok()
                .flatten()
                .map(Value::String)
                .unwrap_or(Value::Null),
        };
        map.insert(col.name().to_string(), value);
    }
    Value::Object(map)
}

fn map_pool_error<E: std::fmt::Display>(err: E) -> ToolError {
    ToolError::internal(format!("PostgreSQL pool error: {}", err))
}

fn map_pg_error(err: tokio_postgres::Error) -> ToolError {
    ToolError::internal(format!("PostgreSQL error: {}", err))
}

#[async_trait]
impl crate::services::tool_executor::ToolHandler for PostgresManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
