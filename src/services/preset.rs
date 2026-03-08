use crate::errors::ToolError;
use crate::services::store_db::StoreDb;
use crate::utils::listing::ListFilters;
use crate::utils::paths::resolve_presets_path;
use serde_json::Value;

const NAMESPACE: &str = "presets";

#[derive(Clone)]
pub struct PresetService {
    store: StoreDb,
}

impl PresetService {
    pub fn new() -> Result<Self, ToolError> {
        let service = Self {
            store: StoreDb::new()?,
        };
        service.import_legacy_once()?;
        Ok(service)
    }

    fn import_legacy_once(&self) -> Result<(), ToolError> {
        let path = resolve_presets_path();
        let import_key = format!("file:{}", path.display());
        if self.store.has_import(NAMESPACE, &import_key)? || !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|err| ToolError::internal(format!("Failed to load presets file: {}", err)))?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|err| ToolError::internal(format!("Failed to parse presets file: {}", err)))?;
        if let Some(obj) = parsed.as_object() {
            for (name, preset) in obj {
                self.validate_preset(preset)?;
                self.store.upsert(NAMESPACE, name, preset, Some("local"))?;
            }
        }
        self.store.mark_imported(NAMESPACE, &import_key)?;
        Ok(())
    }

    fn validate_preset(&self, preset: &Value) -> Result<(), ToolError> {
        let obj = preset
            .as_object()
            .ok_or_else(|| ToolError::invalid_params("preset must be an object"))?;
        if let Some(args) = obj.get("data") {
            if !args.is_object() {
                return Err(ToolError::invalid_params("preset.data must be an object"));
            }
        }
        Ok(())
    }

    pub fn set_preset(&self, name: &str, preset: &Value) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "preset name must be a non-empty string",
            ));
        }
        self.validate_preset(preset)?;
        let existing = self.store.get(NAMESPACE, name)?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut payload = preset.as_object().cloned().unwrap_or_default();
        payload.insert("updated_at".to_string(), Value::String(now.clone()));
        payload.insert(
            "created_at".to_string(),
            existing
                .as_ref()
                .and_then(|v| v.value.get("created_at").cloned())
                .unwrap_or(Value::String(now)),
        );
        self.store.upsert(
            NAMESPACE,
            name.trim(),
            &Value::Object(payload.clone()),
            Some("local"),
        )?;
        let mut out = payload;
        out.insert("name".to_string(), Value::String(name.trim().to_string()));
        Ok(serde_json::json!({"success": true, "preset": Value::Object(out)}))
    }

    pub fn get_preset(&self, name: &str) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "preset name must be a non-empty string",
            ));
        }
        let entry = self.store.get(NAMESPACE, name)?.ok_or_else(|| {
            ToolError::not_found(format!("preset '{}' not found", name))
                .with_hint("Use action=preset_list to see known presets.".to_string())
        })?;
        let mut map = entry.value.as_object().cloned().unwrap_or_default();
        map.insert("name".to_string(), Value::String(name.to_string()));
        Ok(serde_json::json!({"success": true, "preset": Value::Object(map)}))
    }

    pub fn list_presets(&self, filters: &ListFilters) -> Result<Value, ToolError> {
        let mut items = Vec::new();
        for entry in self.store.list(NAMESPACE)? {
            let preset = entry.value;
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String(entry.key.clone()));
            if let Some(tool) = preset.get("tool") {
                map.insert("tool".to_string(), tool.clone());
            }
            if let Some(desc) = preset.get("description") {
                map.insert("description".to_string(), desc.clone());
            }
            map.insert(
                "created_at".to_string(),
                preset.get("created_at").cloned().unwrap_or(Value::Null),
            );
            map.insert(
                "updated_at".to_string(),
                preset.get("updated_at").cloned().unwrap_or(Value::Null),
            );
            items.push(Value::Object(map));
        }
        let result = filters.apply(items, &["name", "description", "tool"], None);
        Ok(serde_json::json!({
            "success": true,
            "presets": result.items,
            "meta": filters.meta(result.total, result.items.len()),
        }))
    }

    pub fn delete_preset(&self, name: &str) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "preset name must be a non-empty string",
            ));
        }
        if !self.store.delete(NAMESPACE, name)? {
            return Err(ToolError::not_found(format!("preset '{}' not found", name))
                .with_hint("Use action=preset_list to see known presets.".to_string()));
        }
        Ok(serde_json::json!({"success": true, "preset": name}))
    }

    pub fn resolve_preset(&self, name: &str) -> Option<Value> {
        self.store
            .get(NAMESPACE, name)
            .ok()
            .flatten()
            .map(|entry| entry.value)
    }

    pub fn get_stats(&self) -> Value {
        let total = self
            .store
            .list(NAMESPACE)
            .map(|items| items.len())
            .unwrap_or(0);
        serde_json::json!({ "total": total })
    }
}
