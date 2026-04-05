use infra::errors::ToolError;
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
use std::sync::{Arc, Mutex};

mod common;
use common::ENV_LOCK;

#[derive(Clone)]
struct DummyHandler {
    calls: Arc<AtomicUsize>,
    seen_args: Arc<Mutex<Vec<Value>>>,
}

#[async_trait::async_trait]
impl ToolHandler for DummyHandler {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen_args
            .lock()
            .expect("seen args lock")
            .push(args.clone());
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
async fn intent_execute_runs_runbook_via_injected_tool_executor() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();
    let prev_runbooks = std::env::var("INFRA_RUNBOOKS_PATH").ok();
    let prev_default_runbooks = std::env::var("INFRA_DEFAULT_RUNBOOKS_PATH").ok();
    let prev_default_capabilities = std::env::var("INFRA_DEFAULT_CAPABILITIES_PATH").ok();

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
                    "inputs": {
                        "required": [],
                        "defaults": {},
                        "map": {}
                    },
                    "when": {},
                    "effects": { "kind": "read", "requires_apply": false }
                }
            }
        }),
    );

    set_env("INFRA_PROFILES_DIR", &tmp_dir);
    set_env("INFRA_RUNBOOKS_PATH", &runbooks_path);
    set_env("INFRA_DEFAULT_RUNBOOKS_PATH", &runbooks_path);
    set_env("INFRA_DEFAULT_CAPABILITIES_PATH", &capabilities_path);

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
        security.clone(),
        validation.clone(),
        capability_service,
        runbook_service,
        evidence_service,
        state_service.clone(),
        None,
        None,
        None,
    ));

    let calls = Arc::new(AtomicUsize::new(0));
    let seen_args = Arc::new(Mutex::new(Vec::new()));
    let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
    handlers.insert(
        "dummy".to_string(),
        Arc::new(DummyHandler {
            calls: calls.clone(),
            seen_args: seen_args.clone(),
        }),
    );
    let tool_executor = Arc::new(ToolExecutor::new(
        logger.clone(),
        state_service,
        None,
        None,
        handlers,
        HashMap::new(),
    ));
    intent_manager.set_tool_executor(tool_executor.clone());

    let result = intent_manager
        .handle_action(serde_json::json!({
            "action": "execute",
            "intent": { "type": "test.echo", "inputs": {} }
        }))
        .await
        .expect("intent execute");

    assert!(result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        result
            .get("plan")
            .and_then(|v| v.get("steps"))
            .and_then(|v| v.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("capability_manifest"))
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert_eq!(
        result
            .get("results")
            .and_then(|v| v.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("capability_manifest"))
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert_eq!(
        result
            .get("plan")
            .and_then(|v| v.get("steps"))
            .and_then(|v| v.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("runbook_manifest"))
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert_eq!(
        result
            .get("results")
            .and_then(|v| v.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("runbook_manifest"))
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );

    restore_env("INFRA_DEFAULT_CAPABILITIES_PATH", prev_default_capabilities);
    restore_env("INFRA_DEFAULT_RUNBOOKS_PATH", prev_default_runbooks);
    restore_env("INFRA_RUNBOOKS_PATH", prev_runbooks);
    restore_env("INFRA_PROFILES_DIR", prev_profiles);
}

#[tokio::test]
async fn intent_execute_prefers_project_runbook_manifest_over_default_manifest() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();
    let prev_runbooks = std::env::var("INFRA_RUNBOOKS_PATH").ok();
    let prev_default_runbooks = std::env::var("INFRA_DEFAULT_RUNBOOKS_PATH").ok();
    let prev_default_capabilities = std::env::var("INFRA_DEFAULT_CAPABILITIES_PATH").ok();

    let tmp_dir = std::env::temp_dir().join(format!("infra-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let default_runbooks_path = tmp_dir.join("defaults-runbooks.json");
    write_json(
        &default_runbooks_path,
        &serde_json::json!({
            "test.echo": {
                "steps": [
                    { "tool": "dummy", "args": { "action": "default" } }
                ]
            }
        }),
    );

    let project_runbooks_path = tmp_dir.join("runbooks.json");
    write_json(
        &project_runbooks_path,
        &serde_json::json!({
            "test.echo": {
                "steps": [
                    { "tool": "dummy", "args": { "action": "project" } }
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
                    "inputs": {
                        "required": [],
                        "defaults": {},
                        "map": {}
                    },
                    "when": {},
                    "effects": { "kind": "read", "requires_apply": false }
                }
            }
        }),
    );

    set_env("INFRA_PROFILES_DIR", &tmp_dir);
    set_env("INFRA_RUNBOOKS_PATH", &project_runbooks_path);
    set_env("INFRA_DEFAULT_RUNBOOKS_PATH", &default_runbooks_path);
    set_env("INFRA_DEFAULT_CAPABILITIES_PATH", &capabilities_path);

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
        security.clone(),
        validation.clone(),
        capability_service,
        runbook_service,
        evidence_service,
        state_service.clone(),
        None,
        None,
        None,
    ));

    let calls = Arc::new(AtomicUsize::new(0));
    let seen_args = Arc::new(Mutex::new(Vec::new()));
    let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
    handlers.insert(
        "dummy".to_string(),
        Arc::new(DummyHandler {
            calls: calls.clone(),
            seen_args: seen_args.clone(),
        }),
    );
    let tool_executor = Arc::new(ToolExecutor::new(
        logger.clone(),
        state_service,
        None,
        None,
        handlers,
        HashMap::new(),
    ));
    intent_manager.set_tool_executor(tool_executor.clone());

    let result = intent_manager
        .handle_action(serde_json::json!({
            "action": "execute",
            "intent": { "type": "test.echo", "inputs": {} }
        }))
        .await
        .expect("intent execute");

    assert!(result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let seen = seen_args.lock().expect("seen args lock");
    assert_eq!(
        seen.last()
            .and_then(|value| value.get("action"))
            .and_then(|value| value.as_str()),
        Some("project")
    );
    assert_eq!(
        result
            .get("results")
            .and_then(|v| v.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("capability_manifest"))
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert_eq!(
        result
            .get("results")
            .and_then(|v| v.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("runbook_manifest"))
            .and_then(|v| v.get("manifest_path"))
            .and_then(|v| v.as_str()),
        Some(project_runbooks_path.to_string_lossy().as_ref())
    );

    restore_env("INFRA_DEFAULT_CAPABILITIES_PATH", prev_default_capabilities);
    restore_env("INFRA_DEFAULT_RUNBOOKS_PATH", prev_default_runbooks);
    restore_env("INFRA_RUNBOOKS_PATH", prev_runbooks);
    restore_env("INFRA_PROFILES_DIR", prev_profiles);
}

#[tokio::test]
async fn intent_execute_explicit_inputs_override_capability_defaults() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();
    let prev_runbooks = std::env::var("INFRA_RUNBOOKS_PATH").ok();
    let prev_default_runbooks = std::env::var("INFRA_DEFAULT_RUNBOOKS_PATH").ok();
    let prev_default_capabilities = std::env::var("INFRA_DEFAULT_CAPABILITIES_PATH").ok();

    let tmp_dir = std::env::temp_dir().join(format!("infra-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let runbooks_path = tmp_dir.join("runbooks.json");
    write_json(
        &runbooks_path,
        &serde_json::json!({
            "test.url": {
                "steps": [
                    {
                        "tool": "dummy",
                        "args": {
                            "action": "echo",
                            "url": "{{ input.url }}"
                        }
                    }
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
                "test.url": {
                    "intent": "test.url",
                    "description": "test url capability",
                    "runbook": "test.url",
                    "tags": ["test"],
                    "inputs": {
                        "required": [],
                        "defaults": {
                            "url": "/health"
                        },
                        "map": {}
                    },
                    "when": {},
                    "effects": { "kind": "read", "requires_apply": false }
                }
            }
        }),
    );

    set_env("INFRA_PROFILES_DIR", &tmp_dir);
    set_env("INFRA_RUNBOOKS_PATH", &runbooks_path);
    set_env("INFRA_DEFAULT_RUNBOOKS_PATH", &runbooks_path);
    set_env("INFRA_DEFAULT_CAPABILITIES_PATH", &capabilities_path);

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
        security.clone(),
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
    let seen_args = Arc::new(Mutex::new(Vec::new()));
    let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
    handlers.insert(
        "dummy".to_string(),
        Arc::new(DummyHandler {
            calls: calls.clone(),
            seen_args: seen_args.clone(),
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

    let explicit_url = "https://example.test/live";
    let result = intent_manager
        .handle_action(serde_json::json!({
            "action": "execute",
            "intent": {
                "type": "test.url",
                "inputs": {
                    "url": explicit_url
                }
            }
        }))
        .await
        .expect("intent execute should keep explicit input");

    assert_eq!(result.get("success").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let seen = seen_args.lock().expect("seen args lock");
    assert_eq!(
        seen.first()
            .and_then(|value| value.get("url"))
            .and_then(|value| value.as_str()),
        Some(explicit_url)
    );

    restore_env("INFRA_DEFAULT_CAPABILITIES_PATH", prev_default_capabilities);
    restore_env("INFRA_DEFAULT_RUNBOOKS_PATH", prev_default_runbooks);
    restore_env("INFRA_RUNBOOKS_PATH", prev_runbooks);
    restore_env("INFRA_PROFILES_DIR", prev_profiles);
}
