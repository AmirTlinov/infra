use crate::errors::ToolError;
use crate::services::store_db::StoreDb;
use crate::utils::listing::ListFilters;
use crate::utils::paths::resolve_projects_path;
use serde_json::Value;

const NAMESPACE: &str = "projects";

#[derive(Clone)]
pub struct ProjectService {
    store: StoreDb,
}

impl ProjectService {
    pub fn new() -> Result<Self, ToolError> {
        let service = Self {
            store: StoreDb::new()?,
        };
        service.import_legacy_once()?;
        Ok(service)
    }

    fn import_legacy_once(&self) -> Result<(), ToolError> {
        let path = resolve_projects_path();
        let import_key = format!("file:{}", path.display());
        if self.store.has_import(NAMESPACE, &import_key)? || !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|err| ToolError::internal(format!("Failed to load projects file: {}", err)))?;
        let parsed: Value = serde_json::from_str(&raw).map_err(|err| {
            ToolError::internal(format!("Failed to parse projects file: {}", err))
        })?;
        if let Some(obj) = parsed.as_object() {
            for (name, project) in obj {
                self.validate_project(project)?;
                self.store.upsert(NAMESPACE, name, project, Some("local"))?;
            }
        }
        self.store.mark_imported(NAMESPACE, &import_key)?;
        Ok(())
    }

    fn validate_project(&self, project: &Value) -> Result<(), ToolError> {
        let obj = project
            .as_object()
            .ok_or_else(|| ToolError::invalid_params("project must be an object"))?;
        if let Some(desc) = obj.get("description") {
            if !desc.is_string() {
                return Err(ToolError::invalid_params(
                    "project.description must be a string",
                ));
            }
        }
        if let Some(default_target) = obj.get("default_target") {
            if default_target
                .as_str()
                .map(|s| s.trim().is_empty())
                .unwrap_or(true)
            {
                return Err(ToolError::invalid_params(
                    "project.default_target must be a non-empty string",
                ));
            }
        }
        if let Some(repo_root) = obj.get("repo_root") {
            if repo_root
                .as_str()
                .map(|s| s.trim().is_empty())
                .unwrap_or(true)
            {
                return Err(ToolError::invalid_params(
                    "project.repo_root must be a non-empty string",
                ));
            }
        }
        if let Some(targets) = obj.get("targets") {
            let targets_obj = targets
                .as_object()
                .ok_or_else(|| ToolError::invalid_params("project.targets must be an object"))?;
            for (name, target) in targets_obj {
                if name.trim().is_empty() {
                    return Err(ToolError::invalid_params(
                        "project.targets keys must be non-empty strings",
                    ));
                }
                self.validate_target(target)?;
            }
        }
        Ok(())
    }

    fn validate_target(&self, target: &Value) -> Result<(), ToolError> {
        let obj = target
            .as_object()
            .ok_or_else(|| ToolError::invalid_params("project.targets entries must be objects"))?;
        for key in [
            "ssh_profile",
            "env_profile",
            "postgres_profile",
            "api_profile",
            "vault_profile",
            "cwd",
            "env_path",
            "description",
        ] {
            if let Some(value) = obj.get(key) {
                if value.as_str().map(|s| s.trim().is_empty()).unwrap_or(true) {
                    return Err(ToolError::invalid_params(format!(
                        "target.{} must be a non-empty string",
                        key
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn set_project(&self, name: &str, project: &Value) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "project name must be a non-empty string",
            ));
        }
        self.validate_project(project)?;
        self.store
            .upsert(NAMESPACE, name.trim(), project, Some("local"))?;
        Ok(serde_json::json!({"success": true, "project": project, "name": name.trim()}))
    }

    pub fn get_project(&self, name: &str) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "project name must be a non-empty string",
            ));
        }
        let project = self.store.get(NAMESPACE, name)?.ok_or_else(|| {
            ToolError::not_found(format!("project '{}' not found", name))
                .with_hint("Use action=project_list to see known projects.".to_string())
        })?;
        Ok(serde_json::json!({"success": true, "project": project.value}))
    }

    pub fn list_projects(&self, filters: &ListFilters) -> Result<Value, ToolError> {
        let mut items = Vec::new();
        for entry in self.store.list(NAMESPACE)? {
            let project = entry.value;
            let mut map = project.as_object().cloned().unwrap_or_default();
            map.insert("name".to_string(), Value::String(entry.key));
            items.push(Value::Object(map));
        }
        let result = filters.apply(items, &["name", "description"], None);
        Ok(serde_json::json!({
            "success": true,
            "projects": result.items,
            "meta": filters.meta(result.total, result.items.len()),
        }))
    }

    pub fn delete_project(&self, name: &str) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "project name must be a non-empty string",
            ));
        }
        if !self.store.delete(NAMESPACE, name)? {
            return Err(
                ToolError::not_found(format!("project '{}' not found", name))
                    .with_hint("Use action=project_list to see known projects.".to_string()),
            );
        }
        Ok(serde_json::json!({"success": true, "project": name}))
    }
}
