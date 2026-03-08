mod common;
use common::ENV_LOCK;

use infra::mcp::aliases::builtin_tool_alias_map_owned;
use infra::mcp::catalog::list_tools_for_openai;
use infra::services::logger::Logger;
use infra::services::state::StateService;
use infra::services::tool_executor::{ToolExecutor, ToolHandler};
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

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
        Ok(serde_json::json!({ "handled": true, "args": args }))
    }
}

#[tokio::test]
async fn tools_list_hides_builtin_aliases_in_full_tier() {
    let _guard = ENV_LOCK.lock().await;

    let prev_infra = std::env::var("INFRA_UNSAFE_LOCAL").ok();

    std::env::remove_var("INFRA_UNSAFE_LOCAL");

    let tools = list_tools_for_openai("full", &HashSet::new());
    let names: HashSet<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();

    assert_eq!(
        names.len(),
        tools.len(),
        "tools/list must not contain duplicate tool names"
    );
    assert!(
        !names.contains("ssh"),
        "ssh alias should be hidden from tools/list in full tier"
    );
    assert!(
        names.contains("mcp_ssh_manager"),
        "canonical ssh tool should remain visible in tools/list"
    );
    assert!(
        !names.contains("sql"),
        "sql alias should be hidden from tools/list in full tier"
    );
    assert!(
        names.contains("mcp_psql_manager"),
        "canonical postgres tool should remain visible in tools/list"
    );
    assert!(
        !names.contains("api"),
        "api alias should be hidden from tools/list in full tier"
    );
    assert!(
        names.contains("mcp_api_client"),
        "canonical api tool should remain visible in tools/list"
    );
    assert!(
        names.contains("mcp_operation"),
        "operation kernel should be discoverable in tools/list"
    );
    assert!(
        !names.contains("mcp_intent"),
        "legacy intent tool should be hidden from tools/list once operation is available"
    );
    assert!(
        !names.contains("mcp_pipeline"),
        "legacy pipeline tool should be hidden from tools/list"
    );

    restore_env("INFRA_UNSAFE_LOCAL", prev_infra);
}

#[tokio::test]
async fn tools_list_hides_local_alias_when_unsafe_local_enabled() {
    let _guard = ENV_LOCK.lock().await;

    let prev_infra = std::env::var("INFRA_UNSAFE_LOCAL").ok();

    std::env::set_var("INFRA_UNSAFE_LOCAL", "1");

    let tools = list_tools_for_openai("full", &HashSet::new());
    let names: HashSet<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();

    assert!(
        !names.contains("local"),
        "local alias should remain hidden even when unsafe local is enabled"
    );

    restore_env("INFRA_UNSAFE_LOCAL", prev_infra);
}

#[tokio::test]
async fn tools_list_does_not_include_aliases_in_core_tier() {
    let _guard = ENV_LOCK.lock().await;

    let core_tools: HashSet<String> = HashSet::from([
        "help".to_string(),
        "legend".to_string(),
        "mcp_capability".to_string(),
        "mcp_operation".to_string(),
        "mcp_receipt".to_string(),
        "mcp_policy".to_string(),
        "mcp_profile".to_string(),
        "mcp_target".to_string(),
    ]);

    let tools = list_tools_for_openai("core", &core_tools);
    let names: HashSet<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();

    assert!(names.contains("mcp_capability"));
    assert!(names.contains("mcp_operation"));
    assert!(names.contains("mcp_receipt"));
    assert!(names.contains("mcp_policy"));
    assert!(names.contains("mcp_profile"));
    assert!(names.contains("mcp_target"));
    assert!(!names.contains("mcp_project"));
    assert!(
        !names.contains("project"),
        "alias tools should not be listed in core tier"
    );
}

#[tokio::test]
async fn explicit_builtin_alias_calls_still_resolve_after_discovery_hides_them() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
    let prev_infra = std::env::var("INFRA_UNSAFE_LOCAL").ok();
    let tmp_dir = std::env::temp_dir().join(format!("infra-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);
    std::env::remove_var("INFRA_UNSAFE_LOCAL");

    let tools = list_tools_for_openai("full", &HashSet::new());
    let names: HashSet<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert!(
        !names.contains("state"),
        "state alias should be hidden from tools/list"
    );
    assert!(
        names.contains("mcp_state"),
        "canonical state tool should stay visible in tools/list"
    );

    let logger = Logger::new("test");
    let state_service = Arc::new(StateService::new().expect("state"));
    let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
    handlers.insert("mcp_state".to_string(), Arc::new(DummyHandler));
    let executor = ToolExecutor::new(
        logger,
        state_service,
        None,
        None,
        handlers,
        builtin_tool_alias_map_owned(),
    );

    let payload = executor
        .execute(
            "state",
            serde_json::json!({
                "action": "get",
                "key": "tool.discovery"
            }),
        )
        .await
        .expect("explicit builtin alias should still execute");

    assert_eq!(
        payload
            .get("meta")
            .and_then(|meta| meta.get("tool"))
            .and_then(|value| value.as_str()),
        Some("mcp_state")
    );
    assert_eq!(
        payload
            .get("meta")
            .and_then(|meta| meta.get("invoked_as"))
            .and_then(|value| value.as_str()),
        Some("state")
    );
    assert_eq!(
        payload
            .get("result")
            .and_then(|result| result.get("handled"))
            .and_then(|value| value.as_bool()),
        Some(true)
    );
    assert_eq!(
        payload
            .get("result")
            .and_then(|result| result.get("args"))
            .and_then(|args| args.get("action"))
            .and_then(|value| value.as_str()),
        Some("get")
    );
    assert_eq!(
        payload
            .get("result")
            .and_then(|result| result.get("args"))
            .and_then(|args| args.get("key"))
            .and_then(|value| value.as_str()),
        Some("tool.discovery")
    );

    restore_env("MCP_PROFILES_DIR", prev_profiles);
    restore_env("INFRA_UNSAFE_LOCAL", prev_infra);
}
