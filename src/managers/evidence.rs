use crate::errors::ToolError;
use crate::services::evidence::EvidenceService;
use crate::services::logger::Logger;
use crate::utils::listing::ListFilters;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

const EVIDENCE_ACTIONS: &[&str] = &["list", "get"];

#[derive(Clone)]
pub struct EvidenceManager {
    logger: Logger,
    evidence_service: Arc<EvidenceService>,
}

impl EvidenceManager {
    pub fn new(logger: Logger, evidence_service: Arc<EvidenceService>) -> Self {
        Self {
            logger: logger.child("evidence"),
            evidence_service,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "list" => {
                let mut filters = ListFilters::from_args(&args);
                if filters.limit.is_none() {
                    filters.limit = Some(20);
                }
                let items: Vec<Value> = self
                    .evidence_service
                    .list_evidence()?
                    .into_iter()
                    .map(Value::String)
                    .collect();
                let result = filters.apply(items, &[], None);
                Ok(serde_json::json!({
                    "success": true,
                    "items": result.items,
                    "meta": filters.meta(result.total, result.items.len()),
                }))
            }
            "get" => {
                let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                self.evidence_service.get_evidence(id)
            }
            _ => Err(unknown_action_error("evidence", action, EVIDENCE_ACTIONS)),
        }
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for EvidenceManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
