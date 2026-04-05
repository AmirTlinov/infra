use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::policy::PolicyService;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

pub(crate) const POLICY_ACTIONS: &[&str] = &["resolve", "evaluate"];

#[derive(Clone)]
pub struct PolicyManager {
    logger: Logger,
    policy_service: Arc<PolicyService>,
}

impl PolicyManager {
    pub fn new(logger: Logger, policy_service: Arc<PolicyService>) -> Self {
        Self {
            logger: logger.child("policy"),
            policy_service,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "resolve" => self.resolve(args),
            "evaluate" | "check" => self.evaluate(args),
            _ => Err(unknown_action_error("policy", action, POLICY_ACTIONS)),
        }
    }

    fn resolve(&self, args: Value) -> Result<Value, ToolError> {
        let inputs = extract_inputs(&args);
        let project_context = args
            .get("project_context")
            .or_else(|| args.get("projectContext"))
            .or_else(|| args.get("context"));
        self.policy_service
            .resolve_effective_policy(inputs.as_ref(), project_context)
    }

    fn evaluate(&self, args: Value) -> Result<Value, ToolError> {
        let inputs = extract_inputs(&args).unwrap_or_else(|| serde_json::json!({}));
        let project_context = args
            .get("project_context")
            .or_else(|| args.get("projectContext"))
            .or_else(|| args.get("context"));
        let intent = find_first_string(&args, &["intent", "capability", "family"]);

        self.policy_service
            .evaluate_effective_policy(intent, &inputs, project_context)
    }
}

fn find_first_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|v| v.as_str()))
}

fn extract_inputs(args: &Value) -> Option<Value> {
    if let Some(inputs) = args.get("inputs") {
        return Some(inputs.clone());
    }
    if let Some(inputs) = args.get("input") {
        return Some(inputs.clone());
    }

    let mut payload = args.as_object()?.clone();
    payload.remove("action");
    payload.remove("project_context");
    payload.remove("projectContext");
    payload.remove("context");
    payload.remove("filters");
    if payload.is_empty() {
        None
    } else {
        Some(Value::Object(payload))
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for PolicyManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
