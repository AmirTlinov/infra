use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::project::ProjectService;
use crate::services::state::StateService;
use crate::services::validation::Validation;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::{Map, Value};
use std::sync::Arc;

const ACTIVE_PROJECT_KEY: &str = "project.active";

pub(crate) const TARGET_ACTIONS: &[&str] = &["list", "get", "resolve"];

#[derive(Clone)]
pub struct TargetManager {
    logger: Logger,
    validation: Validation,
    project_service: Arc<ProjectService>,
    state_service: Arc<StateService>,
}

struct ActiveTargetResolution {
    name: String,
    source: &'static str,
    scope: Option<String>,
}

impl TargetManager {
    pub fn new(
        logger: Logger,
        validation: Validation,
        project_service: Arc<ProjectService>,
        state_service: Arc<StateService>,
    ) -> Self {
        Self {
            logger: logger.child("target"),
            validation,
            project_service,
            state_service,
        }
    }

    fn active_target_key(project_name: &str) -> String {
        format!("target.active.{}", project_name)
    }

    fn resolve_project_name(&self, args: &Value) -> Result<Option<String>, ToolError> {
        if let Some(value) = args.get("project").or_else(|| args.get("project_name")) {
            let project = self.validation.ensure_string(value, "Project name", true)?;
            return self
                .validation
                .ensure_identifier(&project, "Project name")
                .map(Some);
        }

        let state = self.state_service.get(ACTIVE_PROJECT_KEY, Some("any"))?;
        if let Some(project) = state.get("value").and_then(|value| value.as_str()) {
            return self
                .validation
                .ensure_identifier(project, "Project name")
                .map(Some);
        }

        Ok(None)
    }

    fn require_project_name(&self, args: &Value) -> Result<String, ToolError> {
        self.resolve_project_name(args)?.ok_or_else(|| {
            ToolError::invalid_params("project is required")
                .with_hint("Pass args.project (or activate a project first).".to_string())
        })
    }

    fn resolve_target_name(&self, args: &Value) -> Result<String, ToolError> {
        let raw = args
            .get("name")
            .or_else(|| args.get("target_name"))
            .or_else(|| {
                args.get("target").and_then(|value| {
                    if value.is_string() {
                        Some(value)
                    } else {
                        value.get("name")
                    }
                })
            })
            .unwrap_or(&Value::Null);
        let target = self.validation.ensure_string(raw, "Target name", true)?;
        self.validation.ensure_identifier(&target, "Target name")
    }

    fn load_project(&self, project_name: &str) -> Result<Map<String, Value>, ToolError> {
        let payload = self.project_service.get_project(project_name)?;
        payload
            .get("project")
            .and_then(|value| value.as_object())
            .cloned()
            .ok_or_else(|| ToolError::internal("Project service returned an invalid project"))
    }

    fn target_entries(project: &Map<String, Value>) -> Map<String, Value> {
        project
            .get("targets")
            .and_then(|value| value.as_object())
            .cloned()
            .unwrap_or_default()
    }

    fn default_target_name(
        project: &Map<String, Value>,
        targets: &Map<String, Value>,
    ) -> Option<String> {
        if let Some(default_target) = project
            .get("default_target")
            .and_then(|value| value.as_str())
        {
            if targets.contains_key(default_target) {
                return Some(default_target.to_string());
            }
        }
        if targets.len() == 1 {
            return targets.keys().next().cloned();
        }
        None
    }

    fn resolve_active_target(
        &self,
        project_name: &str,
        project: &Map<String, Value>,
    ) -> Result<Option<ActiveTargetResolution>, ToolError> {
        let targets = Self::target_entries(project);
        let state = self
            .state_service
            .get(&Self::active_target_key(project_name), Some("any"))?;
        if let Some(target_name) = state.get("value").and_then(|value| value.as_str()) {
            if targets.contains_key(target_name) {
                return Ok(Some(ActiveTargetResolution {
                    name: target_name.to_string(),
                    source: "state",
                    scope: state
                        .get("scope")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string()),
                }));
            }
        }

        if let Some(target_name) = Self::default_target_name(project, &targets) {
            let source = if project
                .get("default_target")
                .and_then(|value| value.as_str())
                == Some(target_name.as_str())
            {
                "project_default"
            } else {
                "sole_target"
            };
            return Ok(Some(ActiveTargetResolution {
                name: target_name,
                source,
                scope: None,
            }));
        }

        Ok(None)
    }

    fn target_not_found(
        &self,
        project_name: &str,
        target_name: &str,
        targets: &Map<String, Value>,
    ) -> ToolError {
        let mut known_targets: Vec<String> = targets.keys().cloned().collect();
        known_targets.sort();
        let mut err = ToolError::not_found(format!(
            "Target '{}' not found in project '{}'",
            target_name, project_name
        ));
        if !known_targets.is_empty() {
            err = err
                .with_hint(format!("Known targets: {}.", known_targets.join(", ")))
                .with_details(serde_json::json!({ "known_targets": known_targets }));
        } else {
            err = err.with_hint("Project has no targets configured.".to_string());
        }
        err
    }

    fn build_target_record(
        &self,
        project_name: &str,
        project: &Map<String, Value>,
        target_name: &str,
        target: &Value,
        active_name: Option<&str>,
    ) -> Value {
        serde_json::json!({
            "project": project_name,
            "name": target_name,
            "target": target,
            "default": project
                .get("default_target")
                .and_then(|value| value.as_str())
                == Some(target_name),
            "active": active_name == Some(target_name),
        })
    }

    fn resolve_target_record(
        &self,
        project_name: &str,
        project: &Map<String, Value>,
        explicit_target_name: Option<&str>,
    ) -> Result<(String, Value, &'static str, Option<String>), ToolError> {
        let targets = Self::target_entries(project);
        if let Some(target_name) = explicit_target_name {
            let target = targets
                .get(target_name)
                .cloned()
                .ok_or_else(|| self.target_not_found(project_name, target_name, &targets))?;
            return Ok((target_name.to_string(), target, "explicit", None));
        }

        let active = self
            .resolve_active_target(project_name, project)?
            .ok_or_else(|| {
                ToolError::not_found(format!(
                    "No active target is configured for project '{}'",
                    project_name
                ))
                .with_hint(
                    "Pass args.name/args.target_name explicitly, set project.default_target, or store target.active.<project> state."
                        .to_string(),
                )
            })?;
        let target = targets.get(&active.name).cloned().unwrap_or(Value::Null);
        Ok((active.name, target, active.source, active.scope))
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|value| value.as_str()).unwrap_or("") {
            "list" => {
                let project_name = self.require_project_name(&args)?;
                let project = self.load_project(&project_name)?;
                let active = self.resolve_active_target(&project_name, &project)?;
                let active_name = active.as_ref().map(|resolution| resolution.name.as_str());
                let targets = Self::target_entries(&project);
                let mut names: Vec<String> = targets.keys().cloned().collect();
                names.sort();
                let items = names
                    .into_iter()
                    .map(|name| {
                        let target = targets.get(&name).cloned().unwrap_or(Value::Null);
                        self.build_target_record(
                            &project_name,
                            &project,
                            &name,
                            &target,
                            active_name,
                        )
                    })
                    .collect::<Vec<_>>();
                Ok(serde_json::json!({
                    "success": true,
                    "project": project_name,
                    "targets": items,
                }))
            }
            "get" => {
                let project_name = self.require_project_name(&args)?;
                let target_name = self.resolve_target_name(&args)?;
                let project = self.load_project(&project_name)?;
                let active = self.resolve_active_target(&project_name, &project)?;
                let active_name = active.as_ref().map(|resolution| resolution.name.as_str());
                let targets = Self::target_entries(&project);
                let target = targets
                    .get(&target_name)
                    .cloned()
                    .ok_or_else(|| self.target_not_found(&project_name, &target_name, &targets))?;
                Ok(serde_json::json!({
                    "success": true,
                    "project": project_name,
                    "target": self.build_target_record(&project_name, &project, &target_name, &target, active_name),
                }))
            }
            "resolve" => {
                let project_name = self.require_project_name(&args)?;
                let project = self.load_project(&project_name)?;
                let explicit = args
                    .get("name")
                    .or_else(|| args.get("target_name"))
                    .or_else(|| {
                        args.get("target").and_then(|value| {
                            if value.is_string() {
                                Some(value)
                            } else {
                                value.get("name")
                            }
                        })
                    })
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let (target_name, target, source, scope) =
                    self.resolve_target_record(&project_name, &project, explicit)?;

                Ok(serde_json::json!({
                    "success": true,
                    "project": project_name,
                    "target": self.build_target_record(&project_name, &project, &target_name, &target, Some(target_name.as_str())),
                    "source": source,
                    "scope": scope,
                }))
            }
            _ => Err(unknown_action_error("target", action, TARGET_ACTIONS)),
        }
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for TargetManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
