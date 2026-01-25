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
async fn tools_list_includes_builtin_aliases_in_full_tier() {
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
        names.contains("ssh"),
        "ssh alias should be listed in full tier"
    );
    assert!(
        names.contains("sql"),
        "sql alias should be listed in full tier"
    );
    assert!(
        names.contains("api"),
        "api alias should be listed in full tier"
    );
    assert!(
        !names.contains("local"),
        "local alias must be hidden unless INFRA_UNSAFE_LOCAL=1"
    );

    restore_env("INFRA_UNSAFE_LOCAL", prev_infra);
}

#[tokio::test]
async fn tools_list_includes_local_alias_when_unsafe_local_enabled() {
    let _guard = ENV_LOCK.lock().await;

    let prev_infra = std::env::var("INFRA_UNSAFE_LOCAL").ok();

    std::env::set_var("INFRA_UNSAFE_LOCAL", "1");

    let tools = list_tools_for_openai("full", &HashSet::new());
    let names: HashSet<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();

    assert!(
        names.contains("local"),
        "local alias should be listed when unsafe local is enabled"
    );

    restore_env("INFRA_UNSAFE_LOCAL", prev_infra);
}

#[tokio::test]
async fn tools_list_does_not_include_aliases_in_core_tier() {
    let _guard = ENV_LOCK.lock().await;

    let core_tools: HashSet<String> = HashSet::from([
        "help".to_string(),
        "legend".to_string(),
        "mcp_workspace".to_string(),
        "mcp_jobs".to_string(),
        "mcp_artifacts".to_string(),
        "mcp_project".to_string(),
    ]);

    let tools = list_tools_for_openai("core", &core_tools);
    let names: HashSet<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();

    assert!(names.contains("mcp_project"));
    assert!(
        !names.contains("project"),
        "alias tools should not be listed in core tier"
    );
}
