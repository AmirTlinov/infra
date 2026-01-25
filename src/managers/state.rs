use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::state::StateService;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

const STATE_ACTIONS: &[&str] = &["set", "get", "list", "unset", "clear", "dump"];

#[derive(Clone)]
pub struct StateManager {
    logger: Logger,
    state_service: Arc<StateService>,
}

impl StateManager {
    pub fn new(logger: Logger, state_service: Arc<StateService>) -> Self {
        Self {
            logger: logger.child("state"),
            state_service,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "set" => {
                let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let value = args.get("value").cloned().unwrap_or(Value::Null);
                let scope = args.get("scope").and_then(|v| v.as_str());
                self.state_service.set(key, value, scope)
            }
            "get" => {
                let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let scope = args.get("scope").and_then(|v| v.as_str());
                self.state_service.get(key, scope)
            }
            "list" => {
                let prefix = args.get("prefix").and_then(|v| v.as_str());
                let scope = args.get("scope").and_then(|v| v.as_str());
                let include_values = args
                    .get("include_values")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                self.state_service.list(prefix, scope, include_values)
            }
            "unset" => {
                let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let scope = args.get("scope").and_then(|v| v.as_str());
                self.state_service.unset(key, scope)
            }
            "clear" => {
                let scope = args.get("scope").and_then(|v| v.as_str());
                self.state_service.clear(scope)
            }
            "dump" => {
                let scope = args.get("scope").and_then(|v| v.as_str());
                self.state_service.dump(scope)
            }
            _ => Err(unknown_action_error("state", action, STATE_ACTIONS)),
        }
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for StateManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
