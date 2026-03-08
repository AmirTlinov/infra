use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::operation::OperationService;
use crate::services::tool_executor::ToolHandler;
use crate::services::validation::Validation;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

pub(crate) const RECEIPT_ACTIONS: &[&str] = &["list", "get"];

#[derive(Clone)]
pub struct ReceiptManager {
    logger: Logger,
    validation: Validation,
    operation_service: Arc<OperationService>,
}

impl ReceiptManager {
    pub fn new(
        logger: Logger,
        validation: Validation,
        operation_service: Arc<OperationService>,
    ) -> Self {
        Self {
            logger: logger.child("receipt"),
            validation,
            operation_service,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "list" => self.list_receipts(args),
            "get" => self.get_receipt(args),
            _ => Err(unknown_action_error("receipt", action, RECEIPT_ACTIONS)),
        }
    }

    fn list_receipts(&self, args: Value) -> Result<Value, ToolError> {
        let status = self
            .validation
            .ensure_optional_string(args.get("status"), "status", true)?;
        let limit = parse_limit(args.get("limit"))?;

        let mut receipts = self.operation_service.list(usize::MAX, status.as_deref())?;
        let total = receipts.len();
        receipts.truncate(limit);
        let receipts = receipts
            .into_iter()
            .map(|receipt| canonical_receipt_view(&receipt))
            .collect::<Vec<_>>();

        Ok(serde_json::json!({
            "success": true,
            "receipts": receipts,
            "meta": {
                "status": status.map(Value::String).unwrap_or(Value::Null),
                "limit": limit,
                "returned": receipts.len(),
                "total": total,
            }
        }))
    }

    fn get_receipt(&self, args: Value) -> Result<Value, ToolError> {
        let operation_id =
            self.validation
                .ensure_string(receipt_id_value(&args), "operation_id", true)?;
        let Some(receipt) = self.operation_service.get(&operation_id)? else {
            return Err(
                ToolError::not_found(format!("Receipt not found: {}", operation_id)).with_hint(
                    "Use { action: 'list' } to inspect recent persisted receipts.".to_string(),
                ),
            );
        };

        Ok(serde_json::json!({
            "success": true,
            "receipt": canonical_receipt_view(&receipt),
        }))
    }
}

#[async_trait::async_trait]
impl ToolHandler for ReceiptManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}

fn receipt_id_value(args: &Value) -> &Value {
    args.get("operation_id")
        .or_else(|| args.get("id"))
        .unwrap_or(&Value::Null)
}

fn parse_limit(value: Option<&Value>) -> Result<usize, ToolError> {
    let Some(value) = value else {
        return Ok(20);
    };
    if value.is_null() {
        return Ok(20);
    }

    let parsed = value
        .as_u64()
        .or_else(|| value.as_i64().filter(|n| *n > 0).map(|n| n as u64))
        .or_else(|| {
            value
                .as_str()
                .and_then(|text| text.trim().parse::<u64>().ok())
                .filter(|n| *n > 0)
        })
        .ok_or_else(|| ToolError::invalid_params("limit must be a positive integer"))?;

    Ok(parsed.clamp(1, 100) as usize)
}

fn canonical_receipt_view(receipt: &Value) -> Value {
    let effects = receipt.get("effects").cloned().unwrap_or(Value::Null);
    let effect_kind = effects.get("kind").cloned().unwrap_or(Value::Null);

    serde_json::json!({
        "operation_id": clone_or_null(receipt.get("operation_id")),
        "status": clone_or_null(receipt.get("status")),
        "action": clone_or_null(receipt.get("action")),
        "family": clone_or_null(receipt.get("family")),
        "capability": clone_or_null(receipt.get("capability")),
        "capability_manifest": clone_or_null(receipt.get("capability_manifest")),
        "intent": clone_or_null(receipt.get("intent")),
        "runbook": clone_or_null(receipt.get("runbook")),
        "runbook_manifest": clone_or_null(receipt.get("runbook_manifest")),
        "runbook_manifests": array_or_empty(receipt.get("runbook_manifests")),
        "effects": effects,
        "effect_kind": effect_kind,
        "requires_apply": effect_flag(receipt.get("effects"), "requires_apply"),
        "irreversible": effect_flag(receipt.get("effects"), "irreversible"),
        "success": clone_or_null(receipt.get("success")),
        "created_at": clone_or_null(receipt.get("created_at").or_else(|| receipt.get("started_at"))),
        "updated_at": clone_or_null(receipt.get("updated_at")),
        "finished_at": clone_or_null(receipt.get("finished_at")),
        "trace_id": clone_or_null(receipt.get("trace_id")),
        "span_id": clone_or_null(receipt.get("span_id")),
        "parent_span_id": clone_or_null(receipt.get("parent_span_id")),
        "job_ids": array_or_empty(receipt.get("job_ids")),
        "missing": array_or_empty(receipt.get("missing")),
        "summary": clone_or_null(receipt.get("summary")),
    })
}

fn effect_flag(effects: Option<&Value>, key: &str) -> bool {
    effects
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn clone_or_null(value: Option<&Value>) -> Value {
    value.cloned().unwrap_or(Value::Null)
}

fn array_or_empty(value: Option<&Value>) -> Vec<Value> {
    value
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default()
}
