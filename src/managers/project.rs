use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::project::ProjectService;
use crate::services::state::StateService;
use crate::services::validation::Validation;
use crate::utils::listing::ListFilters;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

const ACTIVE_PROJECT_KEY: &str = "project.active";
const PROJECT_ACTIONS: &[&str] = &[
    "project_upsert",
    "project_get",
    "project_list",
    "project_delete",
    "project_use",
    "project_active",
    "project_unuse",
];

#[derive(Clone)]
pub struct ProjectManager {
    logger: Logger,
    validation: Validation,
    project_service: Arc<ProjectService>,
    state_service: Arc<StateService>,
}

impl ProjectManager {
    pub fn new(
        logger: Logger,
        validation: Validation,
        project_service: Arc<ProjectService>,
        state_service: Arc<StateService>,
    ) -> Self {
        Self {
            logger: logger.child("project"),
            validation,
            project_service,
            state_service,
        }
    }

    fn build_project_payload(&self, args: &Value) -> Value {
        if let Some(project) = args.get("project") {
            if project.is_object() {
                return project.clone();
            }
        }
        serde_json::json!({
            "description": args.get("description").cloned().unwrap_or(Value::Null),
            "default_target": args.get("default_target").cloned().unwrap_or(Value::Null),
            "targets": args.get("targets").cloned().unwrap_or(Value::Null),
        })
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "project_upsert" => {
                let name = self.validation.ensure_string(
                    args.get("name").unwrap_or(&Value::Null),
                    "Project name",
                    true,
                )?;
                let payload = self.build_project_payload(&args);
                self.project_service.set_project(&name, &payload)
            }
            "project_get" => {
                let name = self.validation.ensure_string(
                    args.get("name").unwrap_or(&Value::Null),
                    "Project name",
                    true,
                )?;
                self.project_service.get_project(&name)
            }
            "project_list" => {
                let filters = ListFilters::from_args(&args);
                self.project_service.list_projects(&filters)
            }
            "project_delete" => {
                let name = self.validation.ensure_string(
                    args.get("name").unwrap_or(&Value::Null),
                    "Project name",
                    true,
                )?;
                self.project_service.delete_project(&name)
            }
            "project_use" => {
                let name = self.validation.ensure_string(
                    args.get("name").unwrap_or(&Value::Null),
                    "Project name",
                    true,
                )?;
                self.project_service.get_project(&name)?;
                let scope = args
                    .get("scope")
                    .and_then(|v| v.as_str())
                    .unwrap_or("persistent");
                self.state_service.set(
                    ACTIVE_PROJECT_KEY,
                    Value::String(name.clone()),
                    Some(scope),
                )?;
                Ok(serde_json::json!({"success": true, "project": name, "scope": scope}))
            }
            "project_active" => {
                let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("any");
                let state = self.state_service.get(ACTIVE_PROJECT_KEY, Some(scope))?;
                Ok(
                    serde_json::json!({"success": true, "project": state.get("value").cloned().unwrap_or(Value::Null), "scope": state.get("scope").cloned().unwrap_or(Value::Null)}),
                )
            }
            "project_unuse" => {
                let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("any");
                let cleared = self.state_service.unset(ACTIVE_PROJECT_KEY, Some(scope))?;
                Ok(serde_json::json!({"success": true, "cleared": cleared}))
            }
            _ => Err(unknown_action_error("project", action, PROJECT_ACTIONS)),
        }
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for ProjectManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
