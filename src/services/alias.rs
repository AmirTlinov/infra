use crate::errors::ToolError;
use crate::utils::fs_atomic::atomic_write_text_file;
use crate::utils::listing::ListFilters;
use crate::utils::paths::resolve_aliases_path;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct AliasService {
    file_path: std::path::PathBuf,
    aliases: Arc<RwLock<HashMap<String, Value>>>,
}

impl AliasService {
    pub fn new() -> Result<Self, ToolError> {
        let service = Self {
            file_path: resolve_aliases_path(),
            aliases: Arc::new(RwLock::new(HashMap::new())),
        };
        service.load()?;
        Ok(service)
    }

    fn load(&self) -> Result<(), ToolError> {
        if !self.file_path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&self.file_path)
            .map_err(|err| ToolError::internal(format!("Failed to load aliases file: {}", err)))?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|err| ToolError::internal(format!("Failed to parse aliases file: {}", err)))?;
        if let Some(obj) = parsed.as_object() {
            let mut guard = self.aliases.write().unwrap();
            for (name, alias) in obj {
                guard.insert(name.clone(), alias.clone());
            }
        }
        Ok(())
    }

    fn persist(&self) -> Result<(), ToolError> {
        let guard = self.aliases.read().unwrap();
        let payload = serde_json::to_string_pretty(&Value::Object(
            guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        ))
        .map_err(|err| ToolError::internal(format!("Failed to serialize aliases: {}", err)))?;
        atomic_write_text_file(&self.file_path, &format!("{}\n", payload), 0o600)
            .map_err(|err| ToolError::internal(format!("Failed to save aliases: {}", err)))?;
        Ok(())
    }

    fn validate_alias(&self, alias: &Value) -> Result<(), ToolError> {
        let obj = alias
            .as_object()
            .ok_or_else(|| ToolError::invalid_params("alias must be an object"))?;
        let tool = obj.get("tool").and_then(|v| v.as_str()).unwrap_or("");
        if tool.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "alias.tool must be a non-empty string",
            ));
        }
        if let Some(args) = obj.get("args") {
            if !args.is_object() {
                return Err(ToolError::invalid_params("alias.args must be an object"));
            }
        }
        Ok(())
    }

    pub fn set_alias(&self, name: &str, alias: &Value) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "alias name must be a non-empty string",
            ));
        }
        self.validate_alias(alias)?;
        let mut guard = self.aliases.write().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let mut payload = alias.as_object().cloned().unwrap_or_default();
        payload.insert("updated_at".to_string(), Value::String(now.clone()));
        payload.insert(
            "created_at".to_string(),
            guard
                .get(name)
                .and_then(|v| v.get("created_at").cloned())
                .unwrap_or(Value::String(now)),
        );
        guard.insert(name.trim().to_string(), Value::Object(payload.clone()));
        drop(guard);
        self.persist()?;
        let mut out = payload;
        out.insert("name".to_string(), Value::String(name.trim().to_string()));
        Ok(serde_json::json!({"success": true, "alias": Value::Object(out)}))
    }

    pub fn get_alias(&self, name: &str) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "alias name must be a non-empty string",
            ));
        }
        let guard = self.aliases.read().unwrap();
        let entry = guard.get(name).ok_or_else(|| {
            ToolError::not_found(format!("alias '{}' not found", name))
                .with_hint("Use action=alias_list to see known aliases.".to_string())
        })?;
        let mut map = entry.as_object().cloned().unwrap_or_default();
        map.insert("name".to_string(), Value::String(name.to_string()));
        Ok(serde_json::json!({"success": true, "alias": Value::Object(map)}))
    }

    pub fn list_aliases(&self, filters: &ListFilters) -> Result<Value, ToolError> {
        let guard = self.aliases.read().unwrap();
        let mut names: Vec<String> = guard.keys().cloned().collect();
        names.sort();
        let mut items = Vec::new();
        for name in names {
            let alias = match guard.get(&name) {
                Some(value) => value,
                None => continue,
            };
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String(name.clone()));
            if let Some(tool) = alias.get("tool") {
                map.insert("tool".to_string(), tool.clone());
            }
            if let Some(desc) = alias.get("description") {
                map.insert("description".to_string(), desc.clone());
            }
            map.insert(
                "created_at".to_string(),
                alias.get("created_at").cloned().unwrap_or(Value::Null),
            );
            map.insert(
                "updated_at".to_string(),
                alias.get("updated_at").cloned().unwrap_or(Value::Null),
            );
            items.push(Value::Object(map));
        }
        let result = filters.apply(items, &["name", "tool", "description"], None);
        Ok(serde_json::json!({
            "success": true,
            "aliases": result.items,
            "meta": filters.meta(result.total, result.items.len()),
        }))
    }

    pub fn delete_alias(&self, name: &str) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "alias name must be a non-empty string",
            ));
        }
        let mut guard = self.aliases.write().unwrap();
        if guard.remove(name).is_none() {
            return Err(ToolError::not_found(format!("alias '{}' not found", name))
                .with_hint("Use action=alias_list to see known aliases.".to_string()));
        }
        drop(guard);
        self.persist()?;
        Ok(serde_json::json!({"success": true, "alias": name}))
    }

    pub fn resolve_alias(&self, name: &str) -> Option<Value> {
        self.aliases.read().unwrap().get(name).cloned()
    }

    pub fn get_stats(&self) -> Value {
        let total = self.aliases.read().unwrap().len();
        serde_json::json!({ "total": total })
    }
}
