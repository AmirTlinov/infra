use crate::errors::ToolError;
use crate::services::audit::AuditService;
use crate::services::logger::Logger;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

const AUDIT_ACTIONS: &[&str] = &["audit_list", "audit_tail", "audit_clear", "audit_stats"];

#[derive(Clone)]
pub struct AuditManager {
    logger: Logger,
    audit_service: Arc<AuditService>,
}

impl AuditManager {
    pub fn new(logger: Logger, audit_service: Arc<AuditService>) -> Self {
        Self {
            logger: logger.child("audit"),
            audit_service,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "audit_list" => self.audit_service.read_entries(
                args.get("limit").and_then(|v| v.as_i64()).unwrap_or(100) as usize,
                args.get("offset").and_then(|v| v.as_i64()).unwrap_or(0) as usize,
                args.get("reverse")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                &serde_json::json!({
                    "trace_id": args.get("trace_id").cloned().unwrap_or(Value::Null),
                    "tool": args.get("tool").cloned().unwrap_or(Value::Null),
                    "action": args.get("audit_action").cloned().unwrap_or(Value::Null),
                    "status": args.get("status").cloned().unwrap_or(Value::Null),
                    "since": args.get("since").cloned().unwrap_or(Value::Null),
                }),
            ),
            "audit_tail" => self.audit_service.read_entries(
                args.get("limit").and_then(|v| v.as_i64()).unwrap_or(50) as usize,
                0,
                true,
                &serde_json::json!({
                    "trace_id": args.get("trace_id").cloned().unwrap_or(Value::Null),
                    "tool": args.get("tool").cloned().unwrap_or(Value::Null),
                    "action": args.get("audit_action").cloned().unwrap_or(Value::Null),
                    "status": args.get("status").cloned().unwrap_or(Value::Null),
                    "since": args.get("since").cloned().unwrap_or(Value::Null),
                }),
            ),
            "audit_clear" => self.audit_service.clear(),
            "audit_stats" => Ok(serde_json::json!({
                "success": true,
                "stats": self.audit_service.stats(),
            })),
            _ => Err(unknown_action_error("audit", action, AUDIT_ACTIONS)),
        }
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for AuditManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
