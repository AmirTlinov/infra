use crate::errors::ToolError;
use crate::managers::intent::IntentManager;
use crate::managers::runbook::RunbookManager;
use crate::managers::ssh::SshManager;
use crate::services::logger::Logger;
use crate::services::validation::Validation;
use crate::services::workspace::WorkspaceService;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

const WORKSPACE_ACTIONS: &[&str] = &[
    "summary",
    "suggest",
    "diagnose",
    "store_status",
    "run",
    "cleanup",
    "stats",
];

#[derive(Clone)]
pub struct WorkspaceManager {
    logger: Logger,
    validation: Validation,
    workspace_service: Arc<WorkspaceService>,
    runbook_manager: Arc<RunbookManager>,
    intent_manager: Option<Arc<IntentManager>>,
    ssh_manager: Option<Arc<SshManager>>,
}

impl WorkspaceManager {
    pub fn new(
        logger: Logger,
        validation: Validation,
        workspace_service: Arc<WorkspaceService>,
        runbook_manager: Arc<RunbookManager>,
        intent_manager: Option<Arc<IntentManager>>,
        ssh_manager: Option<Arc<SshManager>>,
    ) -> Self {
        Self {
            logger: logger.child("workspace"),
            validation,
            workspace_service,
            runbook_manager,
            intent_manager,
            ssh_manager,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "summary" => {
                let normalized = self.normalize_args(&args)?;
                self.workspace_service.summarize(&normalized).await
            }
            "suggest" => {
                let normalized = self.normalize_args(&args)?;
                self.workspace_service.suggest(&normalized).await
            }
            "diagnose" => {
                let normalized = self.normalize_args(&args)?;
                self.workspace_service.diagnose(&normalized).await
            }
            "store_status" => self.workspace_service.store_status(&args).await,
            "run" => self.run(args).await,
            "cleanup" => self.cleanup().await,
            "stats" => self.workspace_service.stats(&args).await,
            _ => Err(unknown_action_error("workspace", action, WORKSPACE_ACTIONS)),
        }
    }

    fn normalize_args(&self, args: &Value) -> Result<Value, ToolError> {
        let mut payload = args.clone();
        let Some(map) = payload.as_object_mut() else {
            return Ok(payload);
        };

        if let Some(value) = map.get("project") {
            let normalized = self.validation.ensure_string(value, "project", true)?;
            map.insert("project".to_string(), Value::String(normalized));
        }
        if let Some(value) = map.get("target") {
            let normalized = self.validation.ensure_string(value, "target", true)?;
            map.insert("target".to_string(), Value::String(normalized));
        }
        if let Some(value) = map.get("cwd") {
            let normalized = self.validation.ensure_string(value, "cwd", false)?;
            map.insert("cwd".to_string(), Value::String(normalized));
        }
        if let Some(value) = map.get("repo_root") {
            let normalized = self.validation.ensure_string(value, "repo_root", false)?;
            map.insert("repo_root".to_string(), Value::String(normalized));
        }
        if let Some(value) = map.get("key") {
            let normalized = self.validation.ensure_string(value, "key", false)?;
            map.insert("key".to_string(), Value::String(normalized));
        }
        if let Some(value) = map.get("limit").cloned() {
            let parsed = value
                .as_i64()
                .or_else(|| value.as_str().and_then(|s| s.parse::<i64>().ok()));
            if let Some(number) = parsed {
                map.insert(
                    "limit".to_string(),
                    Value::Number(serde_json::Number::from(number)),
                );
            } else {
                map.remove("limit");
            }
        }
        Ok(payload)
    }

    async fn run(&self, args: Value) -> Result<Value, ToolError> {
        let has_intent = args.get("intent").map(|v| !v.is_null()).unwrap_or(false)
            || args.get("intent_type").and_then(|v| v.as_str()).is_some()
            || args.get("type").and_then(|v| v.as_str()).is_some();
        if has_intent {
            let intent_manager = self.intent_manager.as_ref().ok_or_else(|| {
                ToolError::internal("Intent manager is not available").with_hint(
                    "This is a server configuration error. Enable IntentManager in wiring."
                        .to_string(),
                )
            })?;
            let intent_type = args
                .get("intent")
                .and_then(|v| v.get("type"))
                .and_then(|v| v.as_str())
                .or_else(|| args.get("intent_type").and_then(|v| v.as_str()))
                .or_else(|| args.get("type").and_then(|v| v.as_str()))
                .ok_or_else(|| {
                    ToolError::invalid_params("intent type is required").with_hint(
                        "Provide args.intent={ type, inputs } or args.intent_type.".to_string(),
                    )
                })?;

            let inputs = args
                .get("intent")
                .and_then(|v| v.get("inputs"))
                .cloned()
                .or_else(|| args.get("inputs").cloned())
                .or_else(|| args.get("input").cloned())
                .unwrap_or_else(|| Value::Object(Default::default()));

            let intent = serde_json::json!({ "type": intent_type, "inputs": inputs });
            let apply = args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false);
            if apply {
                return intent_manager
                    .handle_action(with_action(&args, "execute", intent))
                    .await;
            }

            let compiled = intent_manager
                .handle_action(with_action(&args, "compile", intent.clone()))
                .await?;
            let requires_apply = compiled
                .get("plan")
                .and_then(|v| v.get("effects"))
                .and_then(|v| v.get("requires_apply"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let action = if requires_apply { "dry_run" } else { "execute" };
            return intent_manager
                .handle_action(with_action(&args, action, intent))
                .await;
        }

        let mut next = args.clone();
        if let Some(map) = next.as_object_mut() {
            map.insert(
                "action".to_string(),
                Value::String("runbook_run".to_string()),
            );
        }
        self.runbook_manager.handle_action(next).await
    }

    async fn cleanup(&self) -> Result<Value, ToolError> {
        let mut results = serde_json::Map::new();
        let mut cleaned = Vec::new();

        let runbook_result = self.runbook_manager.cleanup().await?;
        results.insert("runbook".to_string(), runbook_result);
        cleaned.push("runbook".to_string());

        if let Some(ssh_manager) = self.ssh_manager.as_ref() {
            let result = ssh_manager.cleanup().await?;
            results.insert("ssh".to_string(), result);
            cleaned.push("ssh".to_string());
        }

        Ok(serde_json::json!({
            "success": true,
            "cleaned": cleaned,
            "results": results,
        }))
    }
}

fn with_action(base: &Value, action: &str, intent: Value) -> Value {
    let mut out = base.clone();
    if let Some(map) = out.as_object_mut() {
        map.insert("action".to_string(), Value::String(action.to_string()));
        map.insert("intent".to_string(), intent);
    }
    out
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for WorkspaceManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
