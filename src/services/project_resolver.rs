use crate::errors::ToolError;
use crate::services::state::StateService;
use crate::services::validation::Validation;
use crate::utils::suggest::suggest;
use serde_json::Value;
use std::sync::Arc;

const ACTIVE_PROJECT_KEY: &str = "project.active";

#[derive(Clone)]
pub struct ProjectResolver {
    validation: Validation,
    project_service: Arc<crate::services::project::ProjectService>,
    state_service: Option<Arc<StateService>>,
}

impl ProjectResolver {
    pub fn new(
        validation: Validation,
        project_service: Arc<crate::services::project::ProjectService>,
        state_service: Option<Arc<StateService>>,
    ) -> Self {
        Self {
            validation,
            project_service,
            state_service,
        }
    }

    async fn resolve_project_name(&self, args: &Value) -> Result<Option<String>, ToolError> {
        if let Some(name) = args
            .get("project")
            .or_else(|| args.get("project_name"))
            .and_then(|v| v.as_str())
        {
            return Ok(Some(self.validation.ensure_identifier(name, "project")?));
        }

        if let Some(state) = &self.state_service {
            if let Ok(value) = state.get(ACTIVE_PROJECT_KEY, Some("any")) {
                if let Some(active) = value.get("value").and_then(|v| v.as_str()) {
                    return Ok(Some(self.validation.ensure_identifier(active, "project")?));
                }
            }
        }
        Ok(None)
    }

    fn resolve_target(&self, project: &Value, args: &Value) -> Result<(String, Value), ToolError> {
        let targets = project
            .get("targets")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let requested = args
            .get("target")
            .or_else(|| args.get("project_target"))
            .or_else(|| args.get("environment"))
            .and_then(|v| v.as_str());

        if let Some(requested) = requested {
            let name = self.validation.ensure_identifier(requested, "target")?;
            if let Some(entry) = targets.get(&name) {
                return Ok((name, entry.clone()));
            }
            let known: Vec<String> = targets.keys().cloned().collect();
            let suggestions = suggest(&name, &known, 5);
            let did_you_mean = if suggestions.is_empty() {
                String::new()
            } else {
                format!(" Did you mean: {}?", suggestions.join(", "))
            };
            let hint = if !known.is_empty() {
                format!(" Known targets: {}.", known.join(", "))
            } else {
                String::new()
            };
            let mut err = ToolError::invalid_params(format!("Unknown project target: {}.", name));
            if !(did_you_mean.clone() + &hint).trim().is_empty() {
                err = err.with_hint(format!("{}{}", did_you_mean, hint).trim().to_string());
            }
            if !known.is_empty() {
                err = err.with_details(serde_json::json!({
                    "known_targets": known,
                    "did_you_mean": suggestions,
                }));
            }
            return Err(err);
        }

        if let Some(default_target) = project.get("default_target").and_then(|v| v.as_str()) {
            if let Some(entry) = targets.get(default_target) {
                return Ok((default_target.to_string(), entry.clone()));
            }
        }

        let names: Vec<String> = targets.keys().cloned().collect();
        if names.len() == 1 {
            let name = names[0].clone();
            let entry = targets.get(&name).cloned().unwrap_or(Value::Null);
            return Ok((name, entry));
        }
        if names.is_empty() {
            return Err(ToolError::invalid_params("Project has no targets configured")
                .with_hint(
                    "Add at least one target (project.targets.<name>) or set project.default_target.".to_string(),
                ));
        }
        Err(ToolError::invalid_params(format!(
            "target is required when project has multiple targets (known: {})",
            names.join(", ")
        ))
        .with_hint("Provide args.target (or set project.default_target).".to_string())
        .with_details(serde_json::json!({"known_targets": names})))
    }

    pub async fn resolve_context(&self, args: &Value) -> Result<Option<Value>, ToolError> {
        let project_name = self.resolve_project_name(args).await?;
        let Some(project_name) = project_name else {
            return Ok(None);
        };
        let project = self.project_service.get_project(&project_name)?;
        let project_entry = project.get("project").cloned().unwrap_or(Value::Null);
        let (target_name, target_entry) = self.resolve_target(&project_entry, args)?;
        Ok(Some(serde_json::json!({
            "projectName": project_name,
            "project": project_entry,
            "targetName": target_name,
            "target": target_entry,
        })))
    }
}
