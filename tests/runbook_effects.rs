use infra::errors::ToolErrorKind;
use infra::managers::runbook::RunbookManager;
use infra::services::logger::Logger;
use infra::services::runbook::RunbookService;
use infra::services::state::StateService;
use infra::services::tool_executor::{ToolExecutor, ToolHandler};
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

fn restore_env(key: &str, previous: Option<String>) {
    match previous {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

fn set_env(key: &str, value: &std::path::Path) {
    std::env::set_var(key, value.to_string_lossy().as_ref());
}

#[tokio::test]
async fn runbook_write_requires_apply_and_irreversible_requires_confirm() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
    let prev_default_runbooks = std::env::var("MCP_DEFAULT_RUNBOOKS_PATH").ok();
    let prev_runbooks = std::env::var("MCP_RUNBOOKS_PATH").ok();

    let tmp_dir = std::env::temp_dir().join(format!("infra-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let runbooks_path = tmp_dir.join("runbooks.json");
    write_json(
        &runbooks_path,
        &serde_json::json!({
            "test.write": {
                "tags": ["write"],
                "steps": [
                    { "tool": "dummy", "args": { "action": "ping" } }
                ]
            },
            "test.irreversible": {
                "tags": ["write", "irreversible"],
                "steps": [
                    { "tool": "dummy", "args": { "action": "danger" } }
                ]
            }
        }),
    );

    set_env("MCP_PROFILES_DIR", &tmp_dir);
    set_env("MCP_DEFAULT_RUNBOOKS_PATH", &runbooks_path);
    set_env("MCP_RUNBOOKS_PATH", &runbooks_path);

    let logger = Logger::new("test");
    let state_service = Arc::new(StateService::new().expect("state"));
    let runbook_service = Arc::new(RunbookService::new().expect("runbook service"));

    let calls = Arc::new(AtomicUsize::new(0));
    let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
    handlers.insert(
        "dummy".to_string(),
        Arc::new(DummyHandler {
            calls: calls.clone(),
        }),
    );
    let tool_executor = Arc::new(ToolExecutor::new(
        logger.clone(),
        state_service.clone(),
        None,
        None,
        handlers,
        HashMap::new(),
    ));

    let runbook_manager = RunbookManager::new(logger, runbook_service, state_service);
    runbook_manager.set_tool_executor(tool_executor.clone());

    let err = runbook_manager
        .handle_action(serde_json::json!({
            "action": "runbook_run",
            "name": "test.write",
            "input": {}
        }))
        .await
        .expect_err("write runbook should require apply");
    assert_eq!(err.kind, ToolErrorKind::Denied);
    assert!(err.message.contains("apply=true"));

    let ok = runbook_manager
        .handle_action(serde_json::json!({
            "action": "runbook_run",
            "name": "test.write",
            "apply": true,
            "input": {}
        }))
        .await
        .expect("write runbook apply");
    assert!(ok.get("success").and_then(|v| v.as_bool()).unwrap_or(false));

    let err = runbook_manager
        .handle_action(serde_json::json!({
            "action": "runbook_run",
            "name": "test.irreversible",
            "apply": true,
            "input": {}
        }))
        .await
        .expect_err("irreversible runbook should require confirm");
    assert_eq!(err.kind, ToolErrorKind::Denied);
    assert!(err.message.contains("confirm=true"));

    let ok = runbook_manager
        .handle_action(serde_json::json!({
            "action": "runbook_run",
            "name": "test.irreversible",
            "apply": true,
            "confirm": true,
            "input": {}
        }))
        .await
        .expect("irreversible runbook confirm");
    assert!(ok.get("success").and_then(|v| v.as_bool()).unwrap_or(false));

    assert_eq!(calls.load(Ordering::SeqCst), 2);

    restore_env("MCP_RUNBOOKS_PATH", prev_runbooks);
    restore_env("MCP_DEFAULT_RUNBOOKS_PATH", prev_default_runbooks);
    restore_env("MCP_PROFILES_DIR", prev_profiles);
}

#[tokio::test]
async fn runbook_normal_mode_rejects_compatibility_paths_and_inline_payloads() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
    let prev_runbooks = std::env::var("MCP_RUNBOOKS_PATH").ok();

    let tmp_dir = std::env::temp_dir().join(format!("infra-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let runbooks_path = tmp_dir.join("runbooks.json");
    write_json(
        &runbooks_path,
        &serde_json::json!({
            "test.echo": {
                "steps": [
                    { "tool": "dummy", "args": { "action": "ping" } }
                ]
            }
        }),
    );

    set_env("MCP_PROFILES_DIR", &tmp_dir);
    set_env("MCP_RUNBOOKS_PATH", &runbooks_path);

    let logger = Logger::new("test");
    let state_service = Arc::new(StateService::new().expect("state"));
    let runbook_service = Arc::new(RunbookService::new().expect("runbook service"));

    let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
    handlers.insert(
        "dummy".to_string(),
        Arc::new(DummyHandler {
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    );
    let tool_executor = Arc::new(ToolExecutor::new(
        logger.clone(),
        state_service.clone(),
        None,
        None,
        handlers,
        HashMap::new(),
    ));

    let runbook_manager = RunbookManager::new(logger, runbook_service, state_service);
    runbook_manager.set_tool_executor(tool_executor.clone());

    for (args, expected_stage) in [
        (
            serde_json::json!({ "action": "runbook_upsert", "name": "demo" }),
            "compatibility_runbook_mutation",
        ),
        (
            serde_json::json!({ "action": "runbook_delete", "name": "test.echo" }),
            "compatibility_runbook_mutation",
        ),
        (
            serde_json::json!({ "action": "runbook_compile", "dsl": "step ssh" }),
            "compatibility_runbook_dsl",
        ),
        (
            serde_json::json!({ "action": "runbook_run_dsl", "dsl": "step ssh" }),
            "compatibility_runbook_dsl",
        ),
    ] {
        let err = runbook_manager
            .handle_action(args)
            .await
            .expect_err("compatibility path should be rejected");
        assert_eq!(err.kind, ToolErrorKind::InvalidParams);
        assert!(err.message.contains("compatibility-only"));
        assert_eq!(
            err.details
                .as_ref()
                .and_then(|v| v.get("stage"))
                .and_then(|v| v.as_str()),
            Some(expected_stage)
        );
    }

    let err = runbook_manager
        .handle_action(serde_json::json!({
            "action": "runbook_run",
            "runbook": {
                "steps": [
                    { "tool": "dummy", "args": { "action": "inline" } }
                ]
            },
            "input": {}
        }))
        .await
        .expect_err("inline runbook should be rejected");
    assert_eq!(err.kind, ToolErrorKind::InvalidParams);
    assert!(err.message.contains("compatibility-only"));
    assert_eq!(
        err.details
            .as_ref()
            .and_then(|v| v.get("stage"))
            .and_then(|v| v.as_str()),
        Some("compatibility_runbook_inline")
    );
    assert!(err.hint.as_deref().unwrap_or("").contains("runbooks.json"));

    restore_env("MCP_RUNBOOKS_PATH", prev_runbooks);
    restore_env("MCP_PROFILES_DIR", prev_profiles);
}
