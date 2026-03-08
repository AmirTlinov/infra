use crate::errors::ToolError;
use crate::services::store_db::StoreDb;
use crate::utils::listing::ListFilters;
use crate::utils::paths::resolve_aliases_path;
use serde_json::Value;

const NAMESPACE: &str = "aliases";

#[derive(Clone)]
pub struct AliasService {
    store: StoreDb,
}

impl AliasService {
    pub fn new() -> Result<Self, ToolError> {
        let service = Self {
            store: StoreDb::new()?,
        };
        service.import_legacy_once()?;
        Ok(service)
    }

    fn import_legacy_once(&self) -> Result<(), ToolError> {
        let path = resolve_aliases_path();
        let import_key = format!("file:{}", path.display());
        if self.store.has_import(NAMESPACE, &import_key)? || !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|err| ToolError::internal(format!("Failed to load aliases file: {}", err)))?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|err| ToolError::internal(format!("Failed to parse aliases file: {}", err)))?;
        if let Some(obj) = parsed.as_object() {
            for (name, alias) in obj {
                self.validate_alias(alias)?;
                self.store.upsert(NAMESPACE, name, alias, Some("local"))?;
            }
        }
        self.store.mark_imported(NAMESPACE, &import_key)?;
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
        let existing = self.store.get(NAMESPACE, name)?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut payload = alias.as_object().cloned().unwrap_or_default();
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
        Ok(serde_json::json!({"success": true, "alias": Value::Object(out)}))
    }

    pub fn get_alias(&self, name: &str) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "alias name must be a non-empty string",
            ));
        }
        let entry = self.store.get(NAMESPACE, name)?.ok_or_else(|| {
            ToolError::not_found(format!("alias '{}' not found", name))
                .with_hint("Use action=alias_list to see known aliases.".to_string())
        })?;
        let mut map = entry.value.as_object().cloned().unwrap_or_default();
        map.insert("name".to_string(), Value::String(name.to_string()));
        Ok(serde_json::json!({"success": true, "alias": Value::Object(map)}))
    }

    pub fn list_aliases(&self, filters: &ListFilters) -> Result<Value, ToolError> {
        let mut items = Vec::new();
        for entry in self.store.list(NAMESPACE)? {
            let alias = entry.value;
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String(entry.key.clone()));
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
        if !self.store.delete(NAMESPACE, name)? {
            return Err(ToolError::not_found(format!("alias '{}' not found", name))
                .with_hint("Use action=alias_list to see known aliases.".to_string()));
        }
        Ok(serde_json::json!({"success": true, "alias": name}))
    }

    pub fn resolve_alias(&self, name: &str) -> Option<Value> {
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
