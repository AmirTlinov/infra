use crate::errors::ToolError;
use crate::services::store_db::StoreDb;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

const PERSISTENT_NAMESPACE: &str = "state:persistent";

#[derive(Clone)]
pub struct StateService {
    store: StoreDb,
    session: Arc<RwLock<HashMap<String, Value>>>,
}

impl StateService {
    pub fn new() -> Result<Self, ToolError> {
        let service = Self {
            store: StoreDb::new()?,
            session: Arc::new(RwLock::new(HashMap::new())),
        };
        service.import_legacy_once()?;
        Ok(service)
    }

    fn import_legacy_once(&self) -> Result<(), ToolError> {
        let path = crate::utils::paths::resolve_state_path();
        let import_key = format!("file:{}", path.display());
        if self.store.has_import(PERSISTENT_NAMESPACE, &import_key)? || !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|err| ToolError::internal(format!("Failed to load state file: {}", err)))?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|err| ToolError::internal(format!("Failed to parse state file: {}", err)))?;
        if let Some(obj) = parsed.as_object() {
            for (key, value) in obj {
                self.store
                    .upsert(PERSISTENT_NAMESPACE, key, value, Some("local"))?;
            }
        }
        self.store
            .mark_imported(PERSISTENT_NAMESPACE, &import_key)?;
        Ok(())
    }

    fn normalize_scope(&self, scope: Option<&str>) -> Result<String, ToolError> {
        let normalized = scope.unwrap_or("persistent").to_lowercase();
        match normalized.as_str() {
            "session" | "persistent" | "any" => Ok(normalized),
            _ => Err(ToolError::invalid_params(
                "scope must be one of: session, persistent, any",
            )),
        }
    }

    fn persistent_map(&self) -> Result<HashMap<String, Value>, ToolError> {
        let mut out = HashMap::new();
        for record in self.store.list(PERSISTENT_NAMESPACE)? {
            out.insert(record.key, record.value);
        }
        Ok(out)
    }

    pub fn set(&self, key: &str, value: Value, scope: Option<&str>) -> Result<Value, ToolError> {
        if key.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "State key must be a non-empty string",
            ));
        }
        let normalized = self.normalize_scope(scope)?;
        if normalized == "session" {
            self.session
                .write()
                .unwrap()
                .insert(key.trim().to_string(), value);
        } else {
            self.store
                .upsert(PERSISTENT_NAMESPACE, key.trim(), &value, Some("local"))?;
        }
        Ok(serde_json::json!({
            "success": true,
            "key": key.trim(),
            "scope": if normalized == "any" {"persistent"} else {&normalized}
        }))
    }

    pub fn get(&self, key: &str, scope: Option<&str>) -> Result<Value, ToolError> {
        if key.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "State key must be a non-empty string",
            ));
        }
        let normalized = self.normalize_scope(Some(scope.unwrap_or("any")))?;
        let trimmed = key.trim();
        let mut value = Value::Null;
        let mut resolved_scope = "persistent";
        if normalized == "session" {
            if let Some(val) = self.session.read().unwrap().get(trimmed) {
                value = val.clone();
            }
            resolved_scope = "session";
        } else if normalized == "persistent" {
            if let Some(record) = self.store.get(PERSISTENT_NAMESPACE, trimmed)? {
                value = record.value;
            }
            resolved_scope = "persistent";
        } else if let Some(val) = self.session.read().unwrap().get(trimmed) {
            value = val.clone();
            resolved_scope = "session";
        } else if let Some(record) = self.store.get(PERSISTENT_NAMESPACE, trimmed)? {
            value = record.value;
            resolved_scope = "persistent";
        }
        Ok(
            serde_json::json!({"success": true, "key": trimmed, "value": value, "scope": resolved_scope}),
        )
    }

    pub fn list(
        &self,
        prefix: Option<&str>,
        scope: Option<&str>,
        include_values: bool,
    ) -> Result<Value, ToolError> {
        let normalized = self.normalize_scope(Some(scope.unwrap_or("any")))?;
        let matches_prefix = |key: &str| prefix.map(|p| key.starts_with(p)).unwrap_or(true);
        let gather = |source: &HashMap<String, Value>| {
            let mut keys: Vec<&String> = source
                .keys()
                .filter(|key| matches_prefix(key.as_str()))
                .collect();
            keys.sort();
            keys.into_iter()
                .map(|key| {
                    if include_values {
                        let value = source.get(key).unwrap_or(&Value::Null);
                        serde_json::json!({"key": key, "value": value})
                    } else {
                        serde_json::json!({"key": key})
                    }
                })
                .collect::<Vec<_>>()
        };

        if normalized == "session" {
            return Ok(
                serde_json::json!({"success": true, "scope": "session", "items": gather(&self.session.read().unwrap())}),
            );
        }
        let persistent = self.persistent_map()?;
        if normalized == "persistent" {
            return Ok(
                serde_json::json!({"success": true, "scope": "persistent", "items": gather(&persistent)}),
            );
        }
        let session = self.session.read().unwrap();
        let mut items = gather(&persistent);
        for item in gather(&session) {
            if let Some(key) = item.get("key").and_then(|v| v.as_str()) {
                if !persistent.contains_key(key) {
                    items.push(item);
                }
            }
        }
        items.sort_by(|left, right| {
            let left_key = left.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let right_key = right.get("key").and_then(|v| v.as_str()).unwrap_or("");
            left_key.cmp(right_key)
        });
        Ok(serde_json::json!({"success": true, "scope": "any", "items": items}))
    }

    pub fn unset(&self, key: &str, scope: Option<&str>) -> Result<Value, ToolError> {
        if key.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "State key must be a non-empty string",
            ));
        }
        let normalized = self.normalize_scope(Some(scope.unwrap_or("any")))?;
        let trimmed = key.trim();
        if normalized == "session" || normalized == "any" {
            self.session.write().unwrap().remove(trimmed);
        }
        if normalized == "persistent" || normalized == "any" {
            self.store.delete(PERSISTENT_NAMESPACE, trimmed)?;
        }
        Ok(serde_json::json!({"success": true, "key": trimmed, "scope": normalized}))
    }

    pub fn clear(&self, scope: Option<&str>) -> Result<Value, ToolError> {
        let normalized = self.normalize_scope(Some(scope.unwrap_or("any")))?;
        if normalized == "session" || normalized == "any" {
            self.session.write().unwrap().clear();
        }
        if normalized == "persistent" || normalized == "any" {
            self.store.clear_namespace(PERSISTENT_NAMESPACE)?;
        }
        Ok(serde_json::json!({"success": true, "scope": normalized}))
    }

    pub fn dump(&self, scope: Option<&str>) -> Result<Value, ToolError> {
        let normalized = self.normalize_scope(Some(scope.unwrap_or("any")))?;
        let persistent = self.persistent_map()?;
        let session = self.session.read().unwrap();
        if normalized == "session" {
            return Ok(
                serde_json::json!({"success": true, "scope": "session", "state": session.clone()}),
            );
        }
        if normalized == "persistent" {
            return Ok(
                serde_json::json!({"success": true, "scope": "persistent", "state": persistent.clone()}),
            );
        }
        Ok(serde_json::json!({
            "success": true,
            "scope": "any",
            "state": merge_maps(&persistent, &session),
            "persistent": persistent.clone(),
            "session": session.clone(),
        }))
    }

    pub fn get_stats(&self) -> Value {
        let persistent = self
            .store
            .list(PERSISTENT_NAMESPACE)
            .map(|items| items.len())
            .unwrap_or(0);
        let session = self.session.read().unwrap().len();
        serde_json::json!({
            "session_keys": session,
            "persistent_keys": persistent,
            "store": "sqlite",
            "store_path": self.store.path().display().to_string(),
        })
    }
}

fn merge_maps(a: &HashMap<String, Value>, b: &HashMap<String, Value>) -> HashMap<String, Value> {
    let mut out = a.clone();
    for (k, v) in b.iter() {
        out.insert(k.clone(), v.clone());
    }
    out
}
