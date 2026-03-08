use crate::errors::ToolError;
use crate::services::security::Security;
use crate::services::store_db::StoreDb;
use crate::utils::paths::resolve_profiles_path;
use serde_json::Value;
use std::sync::Arc;

const NAMESPACE: &str = "profiles";

#[derive(Clone)]
pub struct ProfileService {
    security: Arc<Security>,
    store: StoreDb,
}

impl ProfileService {
    pub fn new(security: Arc<Security>) -> Result<Self, ToolError> {
        let service = Self {
            security,
            store: StoreDb::new()?,
        };
        service.import_legacy_once()?;
        Ok(service)
    }

    fn import_legacy_once(&self) -> Result<(), ToolError> {
        let path = resolve_profiles_path();
        let import_key = format!("file:{}", path.display());
        if self.store.has_import(NAMESPACE, &import_key)? || !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|err| ToolError::internal(format!("Failed to load profiles: {}", err)))?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|err| ToolError::internal(format!("Failed to parse profiles: {}", err)))?;
        let obj = parsed
            .as_object()
            .ok_or_else(|| ToolError::invalid_params("Profiles file must be a JSON object"))?;
        for (name, profile) in obj {
            self.validate_stored_profile(name, profile)?;
            self.store.upsert(NAMESPACE, name, profile, Some("local"))?;
        }
        self.store.mark_imported(NAMESPACE, &import_key)?;
        Ok(())
    }

    fn validate_stored_profile(&self, name: &str, profile: &Value) -> Result<(), ToolError> {
        let obj = profile.as_object().ok_or_else(|| {
            ToolError::invalid_params(format!("Profile '{}' has invalid format", name))
        })?;
        let typ = obj.get("type").and_then(|v| v.as_str()).ok_or_else(|| {
            ToolError::invalid_params(format!("Profile '{}' is missing type", name))
        })?;
        if typ.trim().is_empty() {
            return Err(ToolError::invalid_params(format!(
                "Profile '{}' is missing type",
                name
            )));
        }
        if let Some(data) = obj.get("data") {
            if !data.is_object() {
                return Err(ToolError::invalid_params(format!(
                    "Profile '{}' has invalid data section",
                    name
                )));
            }
        }
        if let Some(secrets) = obj.get("secrets") {
            if !secrets.is_object() {
                return Err(ToolError::invalid_params(format!(
                    "Profile '{}' has invalid secrets section",
                    name
                )));
            }
        }
        Ok(())
    }

    pub fn set_profile(&self, name: &str, config: &Value) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "Profile name must be a non-empty string",
            ));
        }
        let config_obj = config
            .as_object()
            .ok_or_else(|| ToolError::invalid_params("Profile config must be an object"))?;
        let existing = self
            .store
            .get(NAMESPACE, name)?
            .map(|record| record.value)
            .unwrap_or_else(|| Value::Object(Default::default()));
        let existing_obj = existing.as_object().cloned().unwrap_or_default();

        let typ = config_obj
            .get("type")
            .and_then(|v| v.as_str())
            .or_else(|| existing_obj.get("type").and_then(|v| v.as_str()))
            .ok_or_else(|| {
                ToolError::invalid_params("Profile type must be specified").with_hint(
                    "Example: { action: \"profile_upsert\", name: \"prod\", type: \"ssh\", data: { host: \"...\" } }".to_string(),
                )
            })?;

        let mut data = existing_obj
            .get("data")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        if let Some(incoming) = config_obj.get("data") {
            if let Some(map) = incoming.as_object() {
                for (key, value) in map {
                    if value.is_null() {
                        data.remove(key);
                    } else {
                        data.insert(key.clone(), value.clone());
                    }
                }
            }
        }

        let mut secrets = existing_obj
            .get("secrets")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        if let Some(secrets_value) = config_obj.get("secrets") {
            if secrets_value.is_null() {
                secrets.clear();
            } else if let Some(map) = secrets_value.as_object() {
                for (key, raw) in map {
                    if raw.is_null() {
                        secrets.remove(key);
                        continue;
                    }
                    let text = raw.as_str().ok_or_else(|| {
                        ToolError::invalid_params(format!("Secret '{}' must be a string", key))
                    })?;
                    let encrypted = self.security.encrypt(text)?;
                    secrets.insert(key.clone(), Value::String(encrypted));
                }
            }
        }

        let now = chrono::Utc::now().to_rfc3339();
        let mut profile = serde_json::json!({
            "type": typ,
            "data": data,
            "created_at": existing_obj.get("created_at").cloned().unwrap_or(Value::String(now.clone())),
            "updated_at": now,
        });
        if !secrets.is_empty() {
            if let Value::Object(map) = &mut profile {
                map.insert("secrets".to_string(), Value::Object(secrets));
            }
        }

        self.store
            .upsert(NAMESPACE, name, &profile, Some("local"))?;

        Ok(serde_json::json!({
            "name": name,
            "type": typ,
            "data": profile.get("data").cloned().unwrap_or(Value::Object(Default::default())),
            "created_at": profile.get("created_at").cloned().unwrap_or(Value::Null),
            "updated_at": profile.get("updated_at").cloned().unwrap_or(Value::Null),
        }))
    }

    pub fn get_profile(&self, name: &str, expected_type: Option<&str>) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "Profile name must be a non-empty string",
            ));
        }
        let entry = self.store.get(NAMESPACE, name)?.ok_or_else(|| {
            ToolError::not_found(format!("Profile '{}' not found", name))
                .with_hint("Use action=profile_list to see known profiles.".to_string())
        })?;
        if let Some(expected) = expected_type {
            if entry.value.get("type").and_then(|v| v.as_str()) != Some(expected) {
                return Err(ToolError::conflict(format!(
                    "Profile '{}' is of type '{}', expected '{}'",
                    name,
                    entry.value.get("type").and_then(|v| v.as_str()).unwrap_or(""),
                    expected
                ))
                .with_hint("Use action=profile_list (optionally filter by type) to locate the correct profile.".to_string()));
            }
        }
        let mut result = serde_json::json!({
            "name": name,
            "type": entry.value.get("type").cloned().unwrap_or(Value::Null),
            "data": entry.value.get("data").cloned().unwrap_or(Value::Object(Default::default())),
        });
        if let Some(secrets) = entry.value.get("secrets").and_then(|v| v.as_object()) {
            let mut decrypted = serde_json::Map::new();
            for (field, value) in secrets {
                let cipher = value.as_str().unwrap_or("");
                let plain = self.security.decrypt(cipher)?;
                decrypted.insert(field.clone(), Value::String(plain));
            }
            if let Value::Object(map) = &mut result {
                map.insert("secrets".to_string(), Value::Object(decrypted));
            }
        }
        Ok(result)
    }

    pub fn list_profiles(&self, filter_type: Option<&str>) -> Result<Value, ToolError> {
        let mut items = Vec::new();
        for entry in self.store.list(NAMESPACE)? {
            let profile = entry.value;
            if let Some(filter) = filter_type {
                if profile.get("type").and_then(|v| v.as_str()) != Some(filter) {
                    continue;
                }
            }
            items.push(serde_json::json!({
                "name": entry.key,
                "type": profile.get("type").cloned().unwrap_or(Value::Null),
                "data": profile.get("data").cloned().unwrap_or(Value::Object(Default::default())),
                "created_at": profile.get("created_at").cloned().unwrap_or(Value::Null),
                "updated_at": profile.get("updated_at").cloned().unwrap_or(Value::Null),
            }));
        }
        Ok(Value::Array(items))
    }

    pub fn delete_profile(&self, name: &str) -> Result<Value, ToolError> {
        if !self.store.delete(NAMESPACE, name)? {
            return Err(
                ToolError::not_found(format!("Profile '{}' not found", name))
                    .with_hint("Use action=profile_list to see known profiles.".to_string()),
            );
        }
        Ok(serde_json::json!({"success": true}))
    }

    pub fn has_profile(&self, name: &str) -> bool {
        if name.trim().is_empty() {
            return false;
        }
        self.store.get(NAMESPACE, name).ok().flatten().is_some()
    }
}
