use crate::errors::ToolError;
use crate::utils::fs_atomic::atomic_write_text_file;
use crate::utils::listing::ListFilters;
use crate::utils::paths::resolve_projects_path;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct ProjectService {
    file_path: std::path::PathBuf,
    projects: Arc<RwLock<HashMap<String, Value>>>,
}

impl ProjectService {
    pub fn new() -> Result<Self, ToolError> {
        let service = Self {
            file_path: resolve_projects_path(),
            projects: Arc::new(RwLock::new(HashMap::new())),
        };
        service.load()?;
        Ok(service)
    }

    fn load(&self) -> Result<(), ToolError> {
        if !self.file_path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&self.file_path)
            .map_err(|err| ToolError::internal(format!("Failed to load projects file: {}", err)))?;
        let parsed: Value = serde_json::from_str(&raw).map_err(|err| {
            ToolError::internal(format!("Failed to parse projects file: {}", err))
        })?;
        let empty = serde_json::Map::new();
        let obj = parsed.as_object().unwrap_or(&empty);
        let mut guard = self.projects.write().unwrap();
        for (name, project) in obj {
            if self.validate_project(project).is_ok() {
                guard.insert(name.clone(), project.clone());
            }
        }
        Ok(())
    }

    fn persist(&self) -> Result<(), ToolError> {
        let guard = self.projects.read().unwrap();
        let data = serde_json::to_string_pretty(&Value::Object(
            guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        ))
        .map_err(|err| ToolError::internal(format!("Failed to serialize projects: {}", err)))?;
        atomic_write_text_file(&self.file_path, &format!("{}\n", data), 0o600)
            .map_err(|err| ToolError::internal(format!("Failed to save projects: {}", err)))?;
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

    fn normalize_name(&self, name: &str) -> Result<String, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "project name must be a non-empty string",
            ));
        }
        Ok(name.trim().to_string())
    }

    pub fn set_project(&self, name: &str, project: &Value) -> Result<Value, ToolError> {
        let trimmed = self.normalize_name(name)?;
        self.validate_project(project)?;
        let mut guard = self.projects.write().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let existing = guard.get(&trimmed).cloned();
        let mut payload = project.as_object().cloned().unwrap_or_default();
        payload.insert("updated_at".to_string(), Value::String(now.clone()));
        payload.insert(
            "created_at".to_string(),
            existing
                .as_ref()
                .and_then(|v| v.get("created_at").cloned())
                .unwrap_or(Value::String(now)),
        );
        let project_value = Value::Object(payload.clone());
        guard.insert(trimmed.clone(), project_value.clone());
        drop(guard);
        self.persist()?;
        let mut project_map = payload.clone();
        project_map.insert("name".to_string(), Value::String(trimmed.clone()));
        Ok(serde_json::json!({"success": true, "project": Value::Object(project_map)}))
    }

    pub fn get_project(&self, name: &str) -> Result<Value, ToolError> {
        let trimmed = self.normalize_name(name)?;
        let guard = self.projects.read().unwrap();
        let project = guard.get(&trimmed).ok_or_else(|| {
            ToolError::not_found(format!("Project '{}' not found", trimmed))
                .with_hint("Use action=project_list to see known projects.".to_string())
        })?;
        let mut project_map = project.as_object().cloned().unwrap_or_default();
        project_map.insert("name".to_string(), Value::String(trimmed));
        Ok(serde_json::json!({"success": true, "project": Value::Object(project_map)}))
    }

    pub fn list_projects(&self, filters: &ListFilters) -> Result<Value, ToolError> {
        let guard = self.projects.read().unwrap();
        let mut out = Vec::new();
        let mut names: Vec<String> = guard.keys().cloned().collect();
        names.sort();
        for name in names {
            let project = guard.get(&name).ok_or_else(|| {
                ToolError::internal("Project disappeared while listing".to_string())
            })?;
            let mut map = project.as_object().cloned().unwrap_or_default();
            map.insert("name".to_string(), Value::String(name));
            out.push(Value::Object(map));
        }
        let result = filters.apply(out, &["name", "description"], None);
        Ok(serde_json::json!({
            "success": true,
            "projects": result.items,
            "meta": filters.meta(result.total, result.items.len()),
        }))
    }

    pub fn delete_project(&self, name: &str) -> Result<Value, ToolError> {
        let trimmed = self.normalize_name(name)?;
        let mut guard = self.projects.write().unwrap();
        if guard.remove(&trimmed).is_none() {
            return Err(
                ToolError::not_found(format!("Project '{}' not found", trimmed))
                    .with_hint("Use action=project_list to see known projects.".to_string()),
            );
        }
        drop(guard);
        self.persist()?;
        Ok(serde_json::json!({"success": true}))
    }
}
