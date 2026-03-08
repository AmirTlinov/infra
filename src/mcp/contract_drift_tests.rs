use crate::managers;
use crate::mcp::aliases::canonical_tool_name;
use crate::mcp::catalog::{discovery_tool_catalog, validate_tool_args};
use crate::mcp::help::build_tool_example;

fn expected_actions_for_canonical_tool(tool: &str) -> Option<&'static [&'static str]> {
    match tool {
        "mcp_alias" => Some(managers::alias::ALIAS_ACTIONS),
        "mcp_preset" => Some(managers::preset::PRESET_ACTIONS),
        "mcp_state" => Some(managers::state::STATE_ACTIONS),
        "mcp_audit" => Some(managers::audit::AUDIT_ACTIONS),
        "mcp_artifacts" => Some(managers::artifacts::ARTIFACT_ACTIONS),
        "mcp_context" => Some(managers::context::CONTEXT_ACTIONS),
        "mcp_project" => Some(managers::project::PROJECT_ACTIONS),
        "mcp_target" => Some(managers::target::TARGET_ACTIONS),
        "mcp_profile" => Some(managers::profile::PROFILE_ACTIONS),
        "mcp_capability" => Some(managers::capability::CAPABILITY_ACTIONS),
        "mcp_operation" => Some(managers::operation::OPERATION_ACTIONS),
        "mcp_receipt" => Some(managers::receipt::RECEIPT_ACTIONS),
        "mcp_policy" => Some(managers::policy::POLICY_ACTIONS),
        "mcp_evidence" => Some(managers::evidence::EVIDENCE_ACTIONS),
        "mcp_workspace" => Some(managers::workspace::WORKSPACE_ACTIONS),
        "mcp_runbook" => Some(managers::runbook::RUNBOOK_ACTIONS),
        "mcp_env" => Some(managers::env::ENV_ACTIONS),
        "mcp_vault" => Some(managers::vault::VAULT_ACTIONS),
        "mcp_ssh_manager" => Some(managers::ssh::SSH_ACTIONS),
        "mcp_api_client" => Some(managers::api::API_ACTIONS),
        "mcp_psql_manager" => Some(managers::postgres::PG_ACTIONS),
        "mcp_local" => Some(managers::local::LOCAL_ACTIONS),
        "mcp_repo" => Some(managers::repo::REPO_ACTIONS),
        "mcp_pipeline" => Some(managers::pipeline::PIPELINE_ACTIONS),
        "mcp_intent" => Some(managers::intent::INTENT_ACTIONS),
        "mcp_jobs" => Some(managers::jobs::JOB_ACTIONS),
        _ => None,
    }
}

fn schema_actions(tool_schema: &serde_json::Value) -> Option<Vec<String>> {
    tool_schema
        .get("properties")
        .and_then(|v| v.get("action"))
        .and_then(|v| v.get("enum"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
}

#[test]
fn contract_drift_schema_actions_match_handlers() {
    for tool in discovery_tool_catalog().iter() {
        let Some(mut schema_actions) = schema_actions(&tool.input_schema) else {
            continue;
        };

        let canonical = canonical_tool_name(&tool.name);
        let Some(expected) = expected_actions_for_canonical_tool(canonical) else {
            panic!(
                "Tool '{}' (canonical='{}') has action enum but no expected action registry entry",
                tool.name, canonical
            );
        };

        schema_actions.sort();
        schema_actions.dedup();

        let mut expected_actions = expected
            .iter()
            .map(|s| (*s).to_string())
            .collect::<Vec<_>>();
        expected_actions.sort();
        expected_actions.dedup();

        assert_eq!(
            schema_actions, expected_actions,
            "Action enum drift for tool '{}' (canonical='{}')",
            tool.name, canonical
        );
    }
}

#[test]
fn contract_drift_examples_validate_against_schema() {
    for tool in discovery_tool_catalog().iter() {
        let Some(schema_actions) = schema_actions(&tool.input_schema) else {
            continue;
        };

        let canonical = canonical_tool_name(&tool.name).to_string();

        for action in schema_actions.iter() {
            let example = build_tool_example(&canonical, action)
                .unwrap_or_else(|| serde_json::json!({ "action": action }));
            validate_tool_args(&tool.name, &example).unwrap_or_else(|err| {
                panic!(
                    "Example drift: tool='{}' (canonical='{}') action='{}' example={} error={}",
                    tool.name, canonical, action, example, err.message
                )
            });
        }
    }
}
