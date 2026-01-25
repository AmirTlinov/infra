use crate::errors::ToolError;
use crate::services::context::ContextService;
use crate::services::logger::Logger;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

const CONTEXT_ACTIONS: &[&str] = &["get", "refresh", "summary", "list", "stats"];

#[derive(Clone)]
pub struct ContextManager {
    logger: Logger,
    context_service: Arc<ContextService>,
}

impl ContextManager {
    pub fn new(logger: Logger, context_service: Arc<ContextService>) -> Self {
        Self {
            logger: logger.child("context"),
            context_service,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "get" => self.context_service.get_context(&args).await,
            "refresh" => {
                let mut next = args.clone();
                if let Value::Object(map) = &mut next {
                    map.insert("refresh".to_string(), Value::Bool(true));
                }
                self.context_service.get_context(&next).await
            }
            "summary" => {
                let mut next = args.clone();
                if let Value::Object(map) = &mut next {
                    map.insert(
                        "refresh".to_string(),
                        Value::Bool(
                            map.get("refresh")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                        ),
                    );
                }
                let result = self.context_service.get_context(&next).await?;
                Ok(serde_json::json!({
                    "success": true,
                    "summary": result.get("context").cloned().unwrap_or(Value::Null),
                }))
            }
            "list" => Ok(serde_json::json!({"success": true, "items": []})),
            "stats" => Ok(serde_json::json!({"success": true, "stats": {}})),
            _ => Err(unknown_action_error("context", action, CONTEXT_ACTIONS)),
        }
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for ContextManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
