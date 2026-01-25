mod common;
use common::ENV_LOCK;

use infra::mcp::catalog::list_tools_for_openai;
use std::collections::HashSet;

fn restore_env(key: &str, previous: Option<String>) {
    match previous {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

#[tokio::test]
async fn tools_list_hides_mcp_local_when_unsafe_local_disabled() {
    let _guard = ENV_LOCK.lock().await;

    let prev_infra = std::env::var("INFRA_UNSAFE_LOCAL").ok();

    std::env::remove_var("INFRA_UNSAFE_LOCAL");

    let tools = list_tools_for_openai("full", &HashSet::new());
    assert!(
        !tools.iter().any(|tool| tool.name == "mcp_local"),
        "mcp_local must be hidden from tools/list unless INFRA_UNSAFE_LOCAL=1"
    );

    restore_env("INFRA_UNSAFE_LOCAL", prev_infra);
}

#[tokio::test]
async fn tools_list_shows_mcp_local_when_unsafe_local_enabled() {
    let _guard = ENV_LOCK.lock().await;

    let prev_infra = std::env::var("INFRA_UNSAFE_LOCAL").ok();

    std::env::set_var("INFRA_UNSAFE_LOCAL", "1");

    let tools = list_tools_for_openai("full", &HashSet::new());
    assert!(
        tools.iter().any(|tool| tool.name == "mcp_local"),
        "mcp_local must be present in tools/list when INFRA_UNSAFE_LOCAL=1"
    );

    restore_env("INFRA_UNSAFE_LOCAL", prev_infra);
}
