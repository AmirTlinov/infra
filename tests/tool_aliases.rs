mod common;
use common::ENV_LOCK;

use infra::services::logger::Logger;
use infra::services::state::StateService;
use infra::services::tool_executor::{ToolExecutor, ToolHandler};
use infra::tooling::names::{builtin_tool_alias_map_owned, canonical_tool_name};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
struct DummyHandler;

#[async_trait::async_trait]
impl ToolHandler for DummyHandler {
    async fn handle(&self, args: Value) -> Result<Value, infra::errors::ToolError> {
        Ok(serde_json::json!({ "handled": true, "args": args }))
    }
}

#[test]
fn short_aliases_canonicalize_to_cli_first_names() {
    assert_eq!(canonical_tool_name("http"), "api");
    assert_eq!(canonical_tool_name("psql"), "sql");
    assert_eq!(canonical_tool_name("postgres"), "sql");
    assert_eq!(canonical_tool_name("state"), "state");
}

#[tokio::test]
async fn short_alias_call_resolves_to_canonical_handler() {
    let _guard = ENV_LOCK.lock().await;

    let logger = Logger::new("test");
    let state_service = Arc::new(StateService::new().expect("state"));
    let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
    handlers.insert("sql".to_string(), Arc::new(DummyHandler));
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
            "psql",
            serde_json::json!({
                "action": "query",
                "sql": "select 1"
            }),
        )
        .await
        .expect("short alias should still execute");

    assert_eq!(
        payload
            .get("meta")
            .and_then(|meta| meta.get("tool"))
            .and_then(|value| value.as_str()),
        Some("sql")
    );
    assert_eq!(
        payload
            .get("meta")
            .and_then(|meta| meta.get("invoked_as"))
            .and_then(|value| value.as_str()),
        Some("psql")
    );
    assert_eq!(
        payload
            .pointer("/result/handled")
            .and_then(|value| value.as_bool()),
        Some(true)
    );
}
