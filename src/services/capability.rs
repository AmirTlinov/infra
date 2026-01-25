use crate::errors::ToolError;
use crate::services::security::Security;
use crate::utils::fs_atomic::atomic_write_text_file;
use crate::utils::paths::{resolve_capabilities_path, resolve_default_capabilities_path};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct CapabilityService {
    security: Arc<Security>,
    file_path: std::path::PathBuf,
    default_path: Option<std::path::PathBuf>,
    capabilities: Arc<RwLock<HashMap<String, Value>>>,
    sources: Arc<RwLock<HashMap<String, String>>>,
}

impl CapabilityService {
    pub fn new(security: Arc<Security>) -> Result<Self, ToolError> {
        let service = Self {
            security,
            file_path: resolve_capabilities_path(),
            default_path: resolve_default_capabilities_path(),
            capabilities: Arc::new(RwLock::new(HashMap::new())),
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
            .map_err(|err| ToolError::internal(format!("Failed to read capabilities: {}", err)))?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|err| ToolError::internal(format!("Failed to parse capabilities: {}", err)))?;
        let entries = parsed.get("capabilities").cloned().unwrap_or(parsed);
        let mut caps = self.capabilities.write().unwrap();
        let mut sources = self.sources.write().unwrap();
        match entries {
            Value::Array(list) => {
                for entry in list {
                    if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                        caps.insert(name.to_string(), entry.clone());
                        sources.insert(name.to_string(), source.to_string());
                    }
                }
            }
            Value::Object(map) => {
                for (name, entry) in map {
                    let mut payload = entry.clone();
                    if let Value::Object(obj) = &mut payload {
                        obj.insert("name".to_string(), Value::String(name.clone()));
                    }
                    caps.insert(name.clone(), payload);
                    sources.insert(name, source.to_string());
                }
            }
            _ => {}
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
        let caps = self.capabilities.read().unwrap();
        let data = serde_json::json!({
            "version": 1,
            "capabilities": caps.clone(),
        });
        let payload = serde_json::to_string_pretty(&data).map_err(|err| {
            ToolError::internal(format!("Failed to serialize capabilities: {}", err))
        })?;
        self.security.ensure_size_fits(&payload, None)?;
        atomic_write_text_file(&self.file_path, &format!("{}\n", payload), 0o600)
            .map_err(|err| ToolError::internal(format!("Failed to save capabilities: {}", err)))?;
        Ok(())
    }

    pub fn list_capabilities(&self) -> Result<Value, ToolError> {
        let caps = self.capabilities.read().unwrap();
        let sources = self.sources.read().unwrap();
        let mut out = Vec::new();
        let mut names: Vec<String> = caps.keys().cloned().collect();
        names.sort();
        for name in names {
            let cap = caps.get(&name).ok_or_else(|| {
                ToolError::internal("Capability disappeared while listing".to_string())
            })?;
            let mut entry = cap.clone();
            if let Value::Object(map) = &mut entry {
                map.insert("name".to_string(), Value::String(name.clone()));
                map.entry("depends_on".to_string())
                    .or_insert_with(|| Value::Array(Vec::new()));
                map.insert(
                    "source".to_string(),
                    Value::String(
                        sources
                            .get(&name)
                            .cloned()
                            .unwrap_or_else(|| "local".to_string()),
                    ),
                );
            }
            out.push(entry);
        }
        Ok(Value::Array(out))
    }

    pub fn get_capability(&self, name: &str) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "Capability name must be a non-empty string",
            ));
        }
        let caps = self.capabilities.read().unwrap();
        let entry = caps.get(name).ok_or_else(|| {
            ToolError::not_found(format!("Capability '{}' not found", name))
                .with_hint("Use action=capability_list to see known capabilities.".to_string())
        })?;
        Ok(entry.clone())
    }

    pub fn find_all_by_intent(&self, intent_type: &str) -> Result<Vec<Value>, ToolError> {
        if intent_type.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "Intent type must be a non-empty string",
            ));
        }
        let caps = self.capabilities.read().unwrap();
        let mut matches = Vec::new();
        if let Some(entry) = caps.get(intent_type) {
            matches.push(entry.clone());
        }
        for cap in caps.values() {
            if cap.get("intent").and_then(|v| v.as_str()) == Some(intent_type) {
                matches.push(cap.clone());
            }
        }
        Ok(matches)
    }

    pub fn set_capability(&self, name: &str, config: &Value) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "Capability name must be a non-empty string",
            ));
        }
        if !config.is_object() {
            return Err(ToolError::invalid_params(
                "Capability config must be an object",
            ));
        }
        let mut caps = self.capabilities.write().unwrap();
        let mut sources = self.sources.write().unwrap();
        let existing = caps
            .get(name)
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let now = chrono::Utc::now().to_rfc3339();
        let mut payload = existing.as_object().cloned().unwrap_or_default();
        if let Some(map) = config.as_object() {
            for (key, value) in map {
                payload.insert(key.clone(), value.clone());
            }
        }
        payload.insert("name".to_string(), Value::String(name.to_string()));
        payload.insert(
            "created_at".to_string(),
            existing
                .get("created_at")
                .cloned()
                .unwrap_or(Value::String(now.clone())),
        );
        payload.insert("updated_at".to_string(), Value::String(now));
        let entry = Value::Object(payload.clone());
        caps.insert(name.to_string(), entry.clone());
        sources.insert(name.to_string(), "local".to_string());
        drop(caps);
        drop(sources);
        self.persist()?;
        Ok(entry)
    }

    pub fn delete_capability(&self, name: &str) -> Result<Value, ToolError> {
        let mut caps = self.capabilities.write().unwrap();
        if caps.remove(name).is_none() {
            return Err(
                ToolError::not_found(format!("Capability '{}' not found", name))
                    .with_hint("Use action=capability_list to see known capabilities.".to_string()),
            );
        }
        drop(caps);
        self.persist()?;
        Ok(serde_json::json!({"success": true}))
    }
}
