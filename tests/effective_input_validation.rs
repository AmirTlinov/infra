use infra::errors::ToolErrorKind;
use infra::services::alias::AliasService;
use infra::services::logger::Logger;
use infra::services::preset::PresetService;
use infra::services::state::StateService;
use infra::services::tool_executor::{ToolExecutor, ToolHandler};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

mod common;
use common::ENV_LOCK;

fn restore_env(key: &str, previous: Option<String>) {
    match previous {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

#[derive(Clone)]
struct DummyHandler;

#[async_trait::async_trait]
impl ToolHandler for DummyHandler {
    async fn handle(&self, args: Value) -> Result<Value, infra::errors::ToolError> {
        Ok(serde_json::json!({ "success": true, "args": args }))
    }
}

#[tokio::test]
async fn alias_injected_fields_are_revalidated_after_merge() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
    let tmp_dir = std::env::temp_dir().join(format!("infra-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);

    let logger = Logger::new("test");
    let state_service = Arc::new(StateService::new().expect("state"));
    let alias_service = Arc::new(AliasService::new().expect("alias service"));
    alias_service
        .set_alias(
            "bad_state",
            &serde_json::json!({
                "tool": "mcp_state",
                "args": {
                    "action": "set",
                    "unexpected": true
                }
            }),
        )
        .expect("set alias");

    let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
    handlers.insert("mcp_state".to_string(), Arc::new(DummyHandler));
    let executor = ToolExecutor::new(
        logger,
        state_service,
        Some(alias_service),
        None,
        handlers,
        HashMap::new(),
    );

    let err = executor
        .execute(
            "bad_state",
            serde_json::json!({
                "key": "k",
                "value": 1,
                "scope": "session"
            }),
        )
        .await
        .expect_err("effective alias args should be revalidated");

    assert_eq!(err.kind, ToolErrorKind::InvalidParams);
    assert!(err.message.contains("unexpected"));
    assert_eq!(
        err.details
            .as_ref()
            .and_then(|v| v.get("stage"))
            .and_then(|v| v.as_str()),
        Some("effective_args")
    );

    restore_env("MCP_PROFILES_DIR", prev_profiles);
}

#[tokio::test]
async fn server_injected_trace_fields_are_ignored_during_effective_validation() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
    let tmp_dir = std::env::temp_dir().join(format!("infra-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);

    let logger = Logger::new("test");
    let state_service = Arc::new(StateService::new().expect("state"));

    let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
    handlers.insert("mcp_state".to_string(), Arc::new(DummyHandler));
    let executor = ToolExecutor::new(logger, state_service, None, None, handlers, HashMap::new());

    let result = executor
        .execute(
            "mcp_state",
            serde_json::json!({
                "action": "set",
                "key": "k",
                "value": 1,
                "scope": "session",
                "trace_id": "trace-1",
                "span_id": "span-1",
                "parent_span_id": "span-0"
            }),
        )
        .await
        .expect("server-injected tracing fields should not fail schema validation");

    assert!(
        result.is_object(),
        "wrapped tool result should still be returned"
    );

    restore_env("MCP_PROFILES_DIR", prev_profiles);
}

#[tokio::test]
async fn preset_args_are_rejected_as_compatibility_only() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
    let tmp_dir = std::env::temp_dir().join(format!("infra-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);

    let logger = Logger::new("test");
    let state_service = Arc::new(StateService::new().expect("state"));
    let preset_service = Arc::new(PresetService::new().expect("preset service"));
    preset_service
        .set_preset(
            "bad_preset",
            &serde_json::json!({
                "tool": "mcp_state",
                "data": {
                    "unexpected": true
                }
            }),
        )
        .expect("set preset");

    let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
    handlers.insert("mcp_state".to_string(), Arc::new(DummyHandler));
    let executor = ToolExecutor::new(logger, state_service, None, None, handlers, HashMap::new());

    let err = executor
        .execute(
            "mcp_state",
            serde_json::json!({
                "action": "set",
                "preset": "bad_preset",
                "key": "k",
                "value": 1,
                "scope": "session"
            }),
        )
        .await
        .expect_err("preset runtime path should be rejected");

    assert_eq!(err.kind, ToolErrorKind::InvalidParams);
    assert!(err.message.contains("compatibility-only"));
    assert_eq!(
        err.details
            .as_ref()
            .and_then(|v| v.get("stage"))
            .and_then(|v| v.as_str()),
        Some("compatibility_preset")
    );

    restore_env("MCP_PROFILES_DIR", prev_profiles);
}

#[tokio::test]
async fn alias_inherited_preset_is_rejected_as_compatibility_only() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
    let tmp_dir = std::env::temp_dir().join(format!("infra-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);

    let logger = Logger::new("test");
    let state_service = Arc::new(StateService::new().expect("state"));
    let alias_service = Arc::new(AliasService::new().expect("alias service"));
    alias_service
        .set_alias(
            "state_with_preset",
            &serde_json::json!({
                "tool": "mcp_state",
                "preset": "legacy_bundle",
                "args": {
                    "action": "set"
                }
            }),
        )
        .expect("set alias");

    let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
    handlers.insert("mcp_state".to_string(), Arc::new(DummyHandler));
    let executor = ToolExecutor::new(
        logger,
        state_service,
        Some(alias_service),
        None,
        handlers,
        HashMap::new(),
    );

    let err = executor
        .execute(
            "state_with_preset",
            serde_json::json!({
                "key": "k",
                "value": 1,
                "scope": "session"
            }),
        )
        .await
        .expect_err("alias preset inheritance should be rejected");

    assert_eq!(err.kind, ToolErrorKind::InvalidParams);
    assert!(err.message.contains("compatibility-only"));
    assert_eq!(
        err.details
            .as_ref()
            .and_then(|v| v.get("stage"))
            .and_then(|v| v.as_str()),
        Some("compatibility_preset")
    );

    restore_env("MCP_PROFILES_DIR", prev_profiles);
}
