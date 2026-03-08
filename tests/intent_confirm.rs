use infra::errors::ToolErrorKind;
use infra::managers::intent::IntentManager;
use infra::services::capability::CapabilityService;
use infra::services::evidence::EvidenceService;
use infra::services::logger::Logger;
use infra::services::runbook::RunbookService;
use infra::services::security::Security;
use infra::services::state::StateService;
use infra::services::tool_executor::{ToolExecutor, ToolHandler};
use infra::services::validation::Validation;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

mod common;
use common::ENV_LOCK;

#[derive(Clone)]
struct DummyHandler {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ToolHandler for DummyHandler {
    async fn handle(&self, args: Value) -> Result<Value, infra::errors::ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(serde_json::json!({ "success": true, "args": args }))
    }
}

fn write_json(path: &std::path::Path, value: &Value) {
    let payload = serde_json::to_string_pretty(value).expect("serialize json");
    std::fs::write(path, format!("{}\n", payload)).expect("write file");
}

fn set_env(key: &str, value: &std::path::Path) {
    std::env::set_var(key, value.to_string_lossy().as_ref());
}

#[tokio::test]
async fn intent_irreversible_requires_confirm_when_apply_true() {
    let _guard = ENV_LOCK.lock().await;

    let tmp_dir = std::env::temp_dir().join(format!("infra-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let runbooks_path = tmp_dir.join("runbooks.json");
    write_json(
        &runbooks_path,
        &serde_json::json!({
            "test.echo": {
                "tags": ["read"],
                "steps": [
                    { "tool": "dummy", "args": { "action": "ping" } }
                ]
            }
        }),
    );

    let capabilities_path = tmp_dir.join("capabilities.json");
    write_json(
        &capabilities_path,
        &serde_json::json!({
            "version": 1,
            "capabilities": {
                "test.echo": {
                    "intent": "test.echo",
                    "description": "test capability",
                    "runbook": "test.echo",
                    "tags": ["test"],
                    "inputs": { "required": [], "defaults": {}, "map": {} },
                    "when": {},
                    "effects": { "kind": "write", "requires_apply": true, "irreversible": true }
                }
            }
        }),
    );

    set_env("MCP_PROFILES_DIR", &tmp_dir);
    set_env("MCP_DEFAULT_RUNBOOKS_PATH", &runbooks_path);
    set_env("MCP_DEFAULT_CAPABILITIES_PATH", &capabilities_path);

    let logger = Logger::new("test");
    let validation = Validation::new();
    let security = Arc::new(Security::new().expect("security"));
    let state_service = Arc::new(StateService::new().expect("state"));

    let capability_service =
        Arc::new(CapabilityService::new(security.clone()).expect("capability service"));
    let runbook_service = Arc::new(RunbookService::new().expect("runbook service"));
    let evidence_service = Arc::new(EvidenceService::new(
        logger.clone(),
        security.as_ref().clone(),
    ));

    let intent_manager = Arc::new(IntentManager::new(
        logger.clone(),
        security,
        validation,
        capability_service,
        runbook_service,
        evidence_service,
        state_service.clone(),
        None,
        None,
        None,
    ));

    let calls = Arc::new(AtomicUsize::new(0));
    let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
    handlers.insert(
        "dummy".to_string(),
        Arc::new(DummyHandler {
            calls: calls.clone(),
        }),
    );
    let tool_executor = Arc::new(ToolExecutor::new(
        logger,
        state_service,
        None,
        None,
        handlers,
        HashMap::new(),
    ));
    intent_manager.set_tool_executor(tool_executor.clone());

    let err = intent_manager
        .handle_action(serde_json::json!({
            "action": "execute",
            "apply": true,
            "intent": { "type": "test.echo", "inputs": {} }
        }))
        .await
        .expect_err("should require confirm");
    assert_eq!(err.kind, ToolErrorKind::Denied);
    assert!(err.message.contains("confirm=true"));

    let ok = intent_manager
        .handle_action(serde_json::json!({
            "action": "execute",
            "apply": true,
            "confirm": true,
            "intent": { "type": "test.echo", "inputs": {} }
        }))
        .await
        .expect("execute with confirm");
    assert!(ok.get("success").and_then(|v| v.as_bool()).unwrap_or(false));

    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
