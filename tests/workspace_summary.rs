use infra::services::alias::AliasService;
use infra::services::capability::CapabilityService;
use infra::services::context::ContextService;
use infra::services::logger::Logger;
use infra::services::preset::PresetService;
use infra::services::profile::ProfileService;
use infra::services::project::ProjectService;
use infra::services::project_resolver::ProjectResolver;
use infra::services::runbook::RunbookService;
use infra::services::security::Security;
use infra::services::state::StateService;
use infra::services::validation::Validation;
use infra::services::workspace::WorkspaceService;
use serde_json::Value;
use std::sync::Arc;

mod common;
use common::ENV_LOCK;

fn write_json(path: &std::path::Path, value: &Value) {
    let payload = serde_json::to_string_pretty(value).expect("serialize json");
    std::fs::write(path, format!("{}\n", payload)).expect("write file");
}

#[tokio::test]
async fn workspace_summary_returns_suggestions() {
    let _guard = ENV_LOCK.lock().await;

    let tmp_dir = std::env::temp_dir().join(format!("infra-workspace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
    let prev_runbooks = std::env::var("MCP_DEFAULT_RUNBOOKS_PATH").ok();
    let prev_caps = std::env::var("MCP_DEFAULT_CAPABILITIES_PATH").ok();

    std::env::set_var("MCP_PROFILES_DIR", tmp_dir.to_string_lossy().as_ref());

    let runbooks_path = tmp_dir.join("runbooks.json");
    write_json(
        &runbooks_path,
        &serde_json::json!({
            "k8s.diff": {
                "description": "Diff",
                "tags": ["k8s"],
                "steps": [{ "tool": "mcp_context", "args": { "action": "context_list" } }]
            }
        }),
    );
    std::env::set_var(
        "MCP_DEFAULT_RUNBOOKS_PATH",
        runbooks_path.to_string_lossy().as_ref(),
    );

    let caps_path = tmp_dir.join("capabilities.json");
    write_json(
        &caps_path,
        &serde_json::json!({
            "version": 1,
            "capabilities": {
                "k8s.diff": {
                    "intent": "k8s.diff",
                    "description": "Diff",
                    "tags": ["k8s"],
                    "when": { "tags_any": ["k8s"] },
                    "inputs": { "required": [], "defaults": {}, "map": {} },
                    "effects": { "kind": "read", "requires_apply": false }
                }
            }
        }),
    );
    std::env::set_var(
        "MCP_DEFAULT_CAPABILITIES_PATH",
        caps_path.to_string_lossy().as_ref(),
    );

    std::fs::create_dir_all(tmp_dir.join(".git")).expect("git dir");
    std::fs::write(
        tmp_dir.join("kustomization.yaml"),
        "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\n",
    )
    .expect("kustomization");

    let logger = Logger::new("test");
    let validation = Validation::new();
    let security = Arc::new(Security::new().expect("security"));
    let state_service = Arc::new(StateService::new().expect("state"));
    let profile_service = Arc::new(ProfileService::new(security.clone()).expect("profile"));
    let project_service = Arc::new(ProjectService::new().expect("project"));
    let project_resolver = Arc::new(ProjectResolver::new(
        validation.clone(),
        project_service.clone(),
        Some(state_service.clone()),
    ));
    let context_service = Arc::new(ContextService::new().expect("context"));
    let runbook_service = Arc::new(RunbookService::new().expect("runbook"));
    let capability_service = Arc::new(CapabilityService::new(security.clone()).expect("cap"));
    let alias_service = Arc::new(AliasService::new().expect("alias"));
    let preset_service = Arc::new(PresetService::new().expect("preset"));

    let workspace = WorkspaceService::new(
        logger.clone(),
        context_service,
        None,
        Some(project_resolver),
        profile_service,
        runbook_service,
        capability_service,
        project_service,
        alias_service,
        preset_service,
        state_service,
    );

    let result = workspace
        .summarize(&serde_json::json!({"cwd": tmp_dir}))
        .await
        .expect("summarize");

    assert!(result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false));
    let capabilities = result
        .pointer("/workspace/suggestions/capabilities")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(capabilities
        .iter()
        .any(|item| { item.get("name").and_then(|v| v.as_str()) == Some("k8s.diff") }));
    let runbooks = result
        .pointer("/workspace/suggestions/runbooks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(runbooks
        .iter()
        .any(|item| { item.get("name").and_then(|v| v.as_str()) == Some("k8s.diff") }));
    let intents = result
        .pointer("/workspace/actions/intents")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(intents
        .iter()
        .any(|item| { item.get("intent").and_then(|v| v.as_str()) == Some("k8s.diff") }));

    let actions_only = workspace
        .summarize(&serde_json::json!({"cwd": tmp_dir, "format": "actions"}))
        .await
        .expect("summarize actions");
    assert!(actions_only
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false));
    assert!(actions_only
        .pointer("/actions/intents")
        .and_then(|v| v.as_array())
        .map(|arr| !arr.is_empty())
        .unwrap_or(false));

    if let Some(prev) = prev_profiles {
        std::env::set_var("MCP_PROFILES_DIR", prev);
    } else {
        std::env::remove_var("MCP_PROFILES_DIR");
    }
    if let Some(prev) = prev_runbooks {
        std::env::set_var("MCP_DEFAULT_RUNBOOKS_PATH", prev);
    } else {
        std::env::remove_var("MCP_DEFAULT_RUNBOOKS_PATH");
    }
    if let Some(prev) = prev_caps {
        std::env::set_var("MCP_DEFAULT_CAPABILITIES_PATH", prev);
    } else {
        std::env::remove_var("MCP_DEFAULT_CAPABILITIES_PATH");
    }
    std::fs::remove_dir_all(&tmp_dir).ok();
}
