use infra::errors::ToolErrorKind;
use infra::services::logger::Logger;
use infra::services::operation::OperationService;
use infra::services::validation::Validation;
use serde_json::Value;
use std::sync::Arc;

mod common;
use common::ENV_LOCK;

mod errors {
    pub use infra::errors::*;
}

mod services {
    pub mod logger {
        pub use infra::services::logger::*;
    }

    pub mod operation {
        pub use infra::services::operation::*;
    }

    pub mod tool_executor {
        pub use infra::services::tool_executor::*;
    }

    pub mod validation {
        pub use infra::services::validation::*;
    }
}

mod utils {
    pub mod tool_errors {
        pub use infra::utils::tool_errors::*;
    }
}

#[path = "../src/managers/receipt.rs"]
mod receipt_impl;

use receipt_impl::ReceiptManager;

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set_path(key: &'static str, value: &std::path::Path) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.as_ref() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn receipt_manager(operation_service: Arc<OperationService>) -> ReceiptManager {
    ReceiptManager::new(Logger::new("test"), Validation::new(), operation_service)
}

fn persist_receipt(
    operation_service: &OperationService,
    operation_id: &str,
    status: &str,
    updated_at: &str,
    payload: Value,
) {
    let mut receipt = serde_json::json!({
        "operation_id": operation_id,
        "status": status,
        "updated_at": updated_at,
    });
    merge_into(&mut receipt, payload);
    operation_service
        .upsert(operation_id, &receipt)
        .expect("persist receipt");
}

fn merge_into(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(target_map), Value::Object(patch_map)) => {
            for (key, value) in patch_map {
                match target_map.get_mut(&key) {
                    Some(existing) => merge_into(existing, value),
                    None => {
                        target_map.insert(key, value);
                    }
                }
            }
        }
        (target, patch) => *target = patch,
    }
}

#[tokio::test]
async fn receipt_list_applies_status_and_limit_filters() {
    let _guard = ENV_LOCK.lock().await;
    let temp_dir =
        std::env::temp_dir().join(format!("infra-receipt-manager-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");
    let _db_guard = EnvGuard::set_path("MCP_STORE_DB_PATH", &temp_dir.join("infra.db"));

    let operation_service = Arc::new(OperationService::new().expect("operation service"));
    persist_receipt(
        operation_service.as_ref(),
        "op-older-success",
        "succeeded",
        "2026-03-07T10:00:00Z",
        serde_json::json!({
            "action": "plan",
            "capability": "demo.plan",
            "effects": {
                "kind": "read",
                "requires_apply": false,
                "irreversible": false
            },
            "summary": "older"
        }),
    );
    persist_receipt(
        operation_service.as_ref(),
        "op-blocked",
        "blocked",
        "2026-03-07T11:00:00Z",
        serde_json::json!({
            "action": "plan",
            "capability": "demo.plan",
            "effects": {
                "kind": "read",
                "requires_apply": false,
                "irreversible": false
            },
            "summary": "blocked"
        }),
    );
    persist_receipt(
        operation_service.as_ref(),
        "op-latest-success",
        "succeeded",
        "2026-03-07T12:00:00Z",
        serde_json::json!({
            "action": "apply",
            "capability": "demo.apply",
            "effects": {
                "kind": "write",
                "requires_apply": true,
                "irreversible": false
            },
            "summary": "latest success"
        }),
    );

    let manager = receipt_manager(operation_service);
    let result = manager
        .handle_action(serde_json::json!({
            "action": "list",
            "status": "succeeded",
            "limit": "1"
        }))
        .await
        .expect("list receipts");

    assert_eq!(result.get("success").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        result
            .get("meta")
            .and_then(|v| v.get("status"))
            .and_then(|v| v.as_str()),
        Some("succeeded")
    );
    assert_eq!(
        result
            .get("meta")
            .and_then(|v| v.get("limit"))
            .and_then(|v| v.as_u64()),
        Some(1)
    );
    assert_eq!(
        result
            .get("meta")
            .and_then(|v| v.get("returned"))
            .and_then(|v| v.as_u64()),
        Some(1)
    );
    assert_eq!(
        result
            .get("meta")
            .and_then(|v| v.get("total"))
            .and_then(|v| v.as_u64()),
        Some(2)
    );

    let receipts = result
        .get("receipts")
        .and_then(|v| v.as_array())
        .expect("receipts array");
    assert_eq!(receipts.len(), 1);
    let latest = &receipts[0];
    assert_eq!(
        latest.get("operation_id").and_then(|v| v.as_str()),
        Some("op-latest-success")
    );
    assert_eq!(
        latest.get("status").and_then(|v| v.as_str()),
        Some("succeeded")
    );
    assert_eq!(
        latest.get("effect_kind").and_then(|v| v.as_str()),
        Some("write")
    );
    assert_eq!(
        latest.get("requires_apply").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        latest.get("summary").and_then(|v| v.as_str()),
        Some("latest success")
    );
}

#[tokio::test]
async fn receipt_get_returns_canonical_typed_view() {
    let _guard = ENV_LOCK.lock().await;
    let temp_dir =
        std::env::temp_dir().join(format!("infra-receipt-manager-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");
    let _db_guard = EnvGuard::set_path("MCP_STORE_DB_PATH", &temp_dir.join("infra.db"));

    let operation_service = Arc::new(OperationService::new().expect("operation service"));
    persist_receipt(
        operation_service.as_ref(),
        "op-get",
        "succeeded",
        "2026-03-07T12:34:56Z",
        serde_json::json!({
            "action": "verify",
            "family": "deploy",
            "capability": "deploy.verify",
            "capability_manifest": {
                "manifest_source": "file_backed_manifest"
            },
            "intent": "deploy.verify",
            "runbook": "deploy.verify",
            "runbook_manifest": {
                "name": "deploy.verify"
            },
            "effects": {
                "kind": "read",
                "requires_apply": false,
                "irreversible": false
            },
            "success": true,
            "created_at": "2026-03-07T12:00:00Z",
            "finished_at": "2026-03-07T12:35:00Z",
            "trace_id": "trace-1",
            "span_id": "span-1",
            "summary": "verification complete"
        }),
    );

    let manager = receipt_manager(operation_service);
    let result = manager
        .handle_action(serde_json::json!({
            "action": "get",
            "id": "op-get"
        }))
        .await
        .expect("get receipt");

    assert_eq!(result.get("success").and_then(|v| v.as_bool()), Some(true));
    let receipt = result.get("receipt").expect("receipt payload");
    assert_eq!(
        receipt.get("operation_id").and_then(|v| v.as_str()),
        Some("op-get")
    );
    assert_eq!(
        receipt.get("action").and_then(|v| v.as_str()),
        Some("verify")
    );
    assert_eq!(
        receipt.get("family").and_then(|v| v.as_str()),
        Some("deploy")
    );
    assert_eq!(
        receipt
            .get("capability_manifest")
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert_eq!(
        receipt.get("effect_kind").and_then(|v| v.as_str()),
        Some("read")
    );
    assert_eq!(
        receipt.get("requires_apply").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        receipt.get("irreversible").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        receipt.get("trace_id").and_then(|v| v.as_str()),
        Some("trace-1")
    );
    assert_eq!(
        receipt.get("span_id").and_then(|v| v.as_str()),
        Some("span-1")
    );
    assert_eq!(
        receipt.get("summary").and_then(|v| v.as_str()),
        Some("verification complete")
    );
    assert_eq!(
        receipt
            .get("runbook_manifests")
            .and_then(|v| v.as_array())
            .map(|items| items.len()),
        Some(0)
    );
    assert_eq!(
        receipt
            .get("job_ids")
            .and_then(|v| v.as_array())
            .map(|items| items.len()),
        Some(0)
    );
    assert_eq!(
        receipt
            .get("missing")
            .and_then(|v| v.as_array())
            .map(|items| items.len()),
        Some(0)
    );
}

#[tokio::test]
async fn receipt_get_returns_not_found_for_missing_operation_id() {
    let _guard = ENV_LOCK.lock().await;
    let temp_dir =
        std::env::temp_dir().join(format!("infra-receipt-manager-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");
    let _db_guard = EnvGuard::set_path("MCP_STORE_DB_PATH", &temp_dir.join("infra.db"));

    let operation_service = Arc::new(OperationService::new().expect("operation service"));
    let manager = receipt_manager(operation_service);
    let err = manager
        .handle_action(serde_json::json!({
            "action": "get",
            "operation_id": "missing-op"
        }))
        .await
        .expect_err("missing receipt should fail");

    assert_eq!(err.kind, ToolErrorKind::NotFound);
    assert_eq!(err.code, "NOT_FOUND");
    assert!(err.message.contains("missing-op"));
}
