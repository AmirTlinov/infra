use infra::mcp::tool_effects::resolve_tool_call_effects;
use serde_json::json;

#[test]
fn state_set_session_is_write_without_apply() {
    let effects = resolve_tool_call_effects(
        "mcp_state",
        &json!({ "action": "set", "scope": "session", "key": "k", "value": 1 }),
    );
    assert_eq!(effects.effects.kind.as_deref(), Some("write"));
    assert!(!effects.effects.requires_apply);
    assert!(!effects.effects.irreversible);
}

#[test]
fn state_set_persistent_requires_apply() {
    let effects = resolve_tool_call_effects(
        "mcp_state",
        &json!({ "action": "set", "scope": "persistent", "key": "k", "value": 1 }),
    );
    assert_eq!(effects.effects.kind.as_deref(), Some("write"));
    assert!(effects.effects.requires_apply);
    assert!(!effects.effects.irreversible);
}

#[test]
fn state_unset_persistent_is_irreversible_and_requires_apply() {
    let effects = resolve_tool_call_effects(
        "mcp_state",
        &json!({ "action": "unset", "scope": "persistent", "key": "k" }),
    );
    assert_eq!(effects.effects.kind.as_deref(), Some("write"));
    assert!(effects.effects.requires_apply);
    assert!(effects.effects.irreversible);
}

#[test]
fn state_clear_persistent_is_irreversible_and_requires_apply() {
    let effects = resolve_tool_call_effects("mcp_state", &json!({ "action": "clear" }));
    // Default scope is persistent.
    assert_eq!(effects.effects.kind.as_deref(), Some("write"));
    assert!(effects.effects.requires_apply);
    assert!(effects.effects.irreversible);
}

#[test]
fn irreversible_implies_apply_even_when_action_default_is_false() {
    // alias_delete is declared as irreversible and historically did not require apply.
    // Our invariant enforces: irreversible => requires_apply.
    let effects = resolve_tool_call_effects(
        "mcp_alias",
        &json!({ "action": "alias_delete", "name": "example" }),
    );
    assert_eq!(effects.effects.kind.as_deref(), Some("write"));
    assert!(effects.effects.requires_apply);
    assert!(effects.effects.irreversible);
}

#[test]
fn api_request_get_is_read() {
    let effects = resolve_tool_call_effects(
        "mcp_api_client",
        &json!({ "action": "request", "method": "GET", "url": "/health" }),
    );
    assert_eq!(effects.effects.kind.as_deref(), Some("read"));
    assert!(!effects.effects.requires_apply);
    assert!(!effects.effects.irreversible);
}

#[test]
fn api_request_post_is_write_requires_apply() {
    let effects = resolve_tool_call_effects(
        "mcp_api_client",
        &json!({ "action": "request", "method": "POST", "url": "/items" }),
    );
    assert_eq!(effects.effects.kind.as_deref(), Some("write"));
    assert!(effects.effects.requires_apply);
    assert!(!effects.effects.irreversible);
}

#[test]
fn api_request_delete_is_irreversible_and_requires_apply() {
    let effects = resolve_tool_call_effects(
        "mcp_api_client",
        &json!({ "action": "request", "method": "DELETE", "url": "/items/1" }),
    );
    assert_eq!(effects.effects.kind.as_deref(), Some("write"));
    assert!(effects.effects.requires_apply);
    assert!(effects.effects.irreversible);
}

#[test]
fn psql_query_cte_select_is_read_without_apply() {
    let effects = resolve_tool_call_effects(
        "mcp_psql_manager",
        &json!({ "action": "query", "sql": "WITH x AS (SELECT 1) SELECT * FROM x" }),
    );
    assert_eq!(effects.effects.kind.as_deref(), Some("read"));
    assert!(!effects.effects.requires_apply);
    assert!(!effects.effects.irreversible);
}

#[test]
fn psql_query_multi_statement_is_mixed_requires_apply() {
    let effects = resolve_tool_call_effects(
        "mcp_psql_manager",
        &json!({ "action": "query", "sql": "SELECT 1; SELECT 2;" }),
    );
    assert_eq!(effects.effects.kind.as_deref(), Some("mixed"));
    assert!(effects.effects.requires_apply);
    assert!(!effects.effects.irreversible);
}

#[test]
fn psql_query_drop_is_irreversible() {
    let effects = resolve_tool_call_effects(
        "mcp_psql_manager",
        &json!({ "action": "query", "sql": "DROP TABLE users" }),
    );
    assert_eq!(effects.effects.kind.as_deref(), Some("write"));
    assert!(effects.effects.requires_apply);
    assert!(effects.effects.irreversible);
}

#[test]
fn compatibility_only_capability_and_runbook_actions_do_not_require_apply() {
    for (tool, action) in [
        ("mcp_capability", "set"),
        ("mcp_capability", "delete"),
        ("mcp_runbook", "runbook_upsert"),
        ("mcp_runbook", "runbook_delete"),
        ("mcp_runbook", "runbook_run_dsl"),
    ] {
        let effects = resolve_tool_call_effects(tool, &json!({ "action": action }));
        assert_eq!(
            effects.effects.kind.as_deref(),
            Some("read"),
            "{tool}:{action}"
        );
        assert!(!effects.effects.requires_apply, "{tool}:{action}");
        assert!(!effects.effects.irreversible, "{tool}:{action}");
    }
}

#[test]
fn canonical_receipt_policy_profile_target_actions_are_read_only() {
    for (tool, args) in [
        ("mcp_receipt", json!({ "action": "list" })),
        (
            "mcp_policy",
            json!({ "action": "evaluate", "intent": "gitops.release" }),
        ),
        (
            "mcp_profile",
            json!({ "action": "get", "name": "prod-api" }),
        ),
        (
            "mcp_target",
            json!({ "action": "resolve", "project": "demo" }),
        ),
    ] {
        let effects = resolve_tool_call_effects(tool, &args);
        assert_eq!(effects.effects.kind.as_deref(), Some("read"), "{tool}");
        assert!(!effects.effects.requires_apply, "{tool}");
        assert!(!effects.effects.irreversible, "{tool}");
    }
}
