use crate::errors::ToolError;
use crate::utils::fs_atomic::atomic_write_text_file;
use crate::utils::listing::ListFilters;
use crate::utils::paths::{resolve_default_runbooks_path, resolve_runbooks_path};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct RunbookService {
    file_path: std::path::PathBuf,
    default_path: Option<std::path::PathBuf>,
    runbooks: Arc<RwLock<HashMap<String, Value>>>,
    sources: Arc<RwLock<HashMap<String, String>>>,
}

impl RunbookService {
    pub fn new() -> Result<Self, ToolError> {
        let service = Self {
            file_path: resolve_runbooks_path(),
            default_path: resolve_default_runbooks_path(),
            runbooks: Arc::new(RwLock::new(HashMap::new())),
            sources: Arc::new(RwLock::new(HashMap::new())),
        };
        service.load()?;
        Ok(service)
    }

    fn load_from_path(&self, path: &std::path::Path, source: &str) -> Result<(), ToolError> {
        if !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|err| ToolError::internal(format!("Failed to load runbooks: {}", err)))?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|err| ToolError::internal(format!("Failed to parse runbooks: {}", err)))?;
        if let Some(obj) = parsed.as_object() {
            let mut guard = self.runbooks.write().unwrap();
            let mut sources = self.sources.write().unwrap();
            for (name, runbook) in obj {
                guard.insert(name.clone(), runbook.clone());
                sources.insert(name.clone(), source.to_string());
            }
        }
        Ok(())
    }

    fn load(&self) -> Result<(), ToolError> {
        if let Some(default_path) = self.default_path.as_ref() {
            let _ = self.load_from_path(default_path, "default");
        }
        let _ = self.load_from_path(&self.file_path, "local");
        Ok(())
    }

    fn persist(&self) -> Result<(), ToolError> {
        let guard = self.runbooks.read().unwrap();
        let payload = serde_json::to_string_pretty(&Value::Object(
            guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        ))
        .map_err(|err| ToolError::internal(format!("Failed to serialize runbooks: {}", err)))?;
        atomic_write_text_file(&self.file_path, &format!("{}\n", payload), 0o600)
            .map_err(|err| ToolError::internal(format!("Failed to save runbooks: {}", err)))?;
        Ok(())
    }

    fn validate_runbook(&self, runbook: &Value) -> Result<(), ToolError> {
        let obj = runbook
            .as_object()
            .ok_or_else(|| ToolError::invalid_params("runbook must be an object"))?;
        let empty_steps = Vec::new();
        let steps = obj
            .get("steps")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty_steps);
        if steps.is_empty() {
            return Err(
                ToolError::invalid_params("runbook.steps must be a non-empty array").with_hint(
                    "Provide at least one step: [{ tool: \"ssh\", args: { ... } }].".to_string(),
                ),
            );
        }
        Ok(())
    }

    pub fn set_runbook(&self, name: &str, runbook: &Value) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "runbook name must be a non-empty string",
            ));
        }
        self.validate_runbook(runbook)?;
        let mut guard = self.runbooks.write().unwrap();
        let mut sources = self.sources.write().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let mut payload = runbook.as_object().cloned().unwrap_or_default();
        payload.insert("updated_at".to_string(), Value::String(now.clone()));
        payload.insert(
            "created_at".to_string(),
            guard
                .get(name)
                .and_then(|v| v.get("created_at").cloned())
                .unwrap_or(Value::String(now)),
        );
        guard.insert(name.trim().to_string(), Value::Object(payload.clone()));
        sources.insert(name.trim().to_string(), "local".to_string());
        drop(guard);
        drop(sources);
        self.persist()?;
        let mut out = payload;
        out.insert("name".to_string(), Value::String(name.trim().to_string()));
        Ok(serde_json::json!({"success": true, "runbook": Value::Object(out)}))
    }

    pub fn get_runbook(&self, name: &str) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "runbook name must be a non-empty string",
            ));
        }
        let guard = self.runbooks.read().unwrap();
        let sources = self.sources.read().unwrap();
        let entry = guard.get(name).ok_or_else(|| {
            ToolError::not_found(format!("runbook '{}' not found", name))
                .with_hint("Use action=runbook_list to see known runbooks.".to_string())
        })?;
        let mut map = entry.as_object().cloned().unwrap_or_default();
        map.insert("name".to_string(), Value::String(name.to_string()));
        map.insert(
            "source".to_string(),
            Value::String(
                sources
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| "local".to_string()),
            ),
        );
        Ok(serde_json::json!({"success": true, "runbook": Value::Object(map)}))
    }

    pub fn list_runbooks(&self, filters: &ListFilters) -> Result<Value, ToolError> {
        let guard = self.runbooks.read().unwrap();
        let sources = self.sources.read().unwrap();
        let mut names: Vec<String> = guard.keys().cloned().collect();
        names.sort();
        let mut items = Vec::new();
        for name in names {
            let runbook = match guard.get(&name) {
                Some(value) => value,
                None => continue,
            };
            let steps_len = runbook
                .get("steps")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len())
                .unwrap_or(0);
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String(name.clone()));
            if let Some(desc) = runbook.get("description") {
                if !desc.is_null() {
                    map.insert("description".to_string(), desc.clone());
                }
            }
            let tags = runbook
                .get("tags")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            map.insert("tags".to_string(), Value::Array(tags));
            if let Some(when) = runbook.get("when") {
                map.insert("when".to_string(), when.clone());
            }
            if let Some(inputs) = runbook.get("inputs") {
                if !inputs.is_null() {
                    map.insert("inputs".to_string(), inputs.clone());
                }
            }
            map.insert(
                "steps".to_string(),
                Value::Number(serde_json::Number::from(steps_len as i64)),
            );
            if let Some(created_at) = runbook.get("created_at") {
                if !created_at.is_null() {
                    map.insert("created_at".to_string(), created_at.clone());
                }
            }
            if let Some(updated_at) = runbook.get("updated_at") {
                if !updated_at.is_null() {
                    map.insert("updated_at".to_string(), updated_at.clone());
                }
            }
            map.insert(
                "source".to_string(),
                Value::String(
                    sources
                        .get(&name)
                        .cloned()
                        .unwrap_or_else(|| "local".to_string()),
                ),
            );
            items.push(Value::Object(map));
        }
        let result = filters.apply(items, &["name", "description", "tags"], Some("tags"));
        Ok(serde_json::json!({
            "success": true,
            "runbooks": result.items,
            "meta": filters.meta(result.total, result.items.len()),
        }))
    }

    pub fn delete_runbook(&self, name: &str) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "runbook name must be a non-empty string",
            ));
        }
        let mut guard = self.runbooks.write().unwrap();
        if guard.remove(name).is_none() {
            return Err(
                ToolError::not_found(format!("runbook '{}' not found", name))
                    .with_hint("Use action=runbook_list to see known runbooks.".to_string()),
            );
        }
        drop(guard);
        self.persist()?;
        Ok(serde_json::json!({"success": true, "runbook": name}))
    }
}
