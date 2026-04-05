use infra::errors::ToolError;
use infra::managers::intent::IntentManager;
use infra::managers::operation::OperationManager;
use infra::services::capability::CapabilityService;
use infra::services::context::ContextService;
use infra::services::evidence::EvidenceService;
use infra::services::job::JobService;
use infra::services::logger::Logger;
use infra::services::operation::OperationService;
use infra::services::runbook::RunbookService;
use infra::services::security::Security;
use infra::services::state::StateService;
use infra::services::tool_executor::{ToolExecutor, ToolHandler};
use infra::services::validation::Validation;
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
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(serde_json::json!({ "success": true, "args": args }))
    }
}

fn restore_env(key: &str, previous: Option<String>) {
    match previous {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

fn write_json(path: &std::path::Path, value: &Value) {
    let payload = serde_json::to_string_pretty(value).expect("serialize json");
    std::fs::write(path, format!("{}\n", payload)).expect("write file");
}

fn set_env(key: &str, value: &std::path::Path) {
    std::env::set_var(key, value.to_string_lossy().as_ref());
}

#[tokio::test]
async fn operation_plan_and_apply_flow_persist_receipts() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();
    let prev_default_runbooks = std::env::var("INFRA_DEFAULT_RUNBOOKS_PATH").ok();
    let prev_default_capabilities = std::env::var("INFRA_DEFAULT_CAPABILITIES_PATH").ok();
    let prev_capabilities = std::env::var("INFRA_CAPABILITIES_PATH").ok();
    let prev_store_db = std::env::var("INFRA_STORE_DB_PATH").ok();

    let tmp_dir =
        std::env::temp_dir().join(format!("infra-operation-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let runbooks_path = tmp_dir.join("runbooks.json");
    write_json(
        &runbooks_path,
        &serde_json::json!({
            "demo.plan": {
                "effects": { "kind": "read", "requires_apply": false, "irreversible": false },
                "steps": [
                    { "tool": "dummy", "args": { "action": "preview" } }
                ]
            },
            "demo.prepare": {
                "effects": { "kind": "write", "requires_apply": true, "irreversible": false },
                "steps": [
                    { "tool": "dummy", "args": { "action": "prepare" } }
                ]
            },
            "demo.sync": {
                "effects": { "kind": "write", "requires_apply": true, "irreversible": false },
                "steps": [
                    { "tool": "dummy", "args": { "action": "apply" } }
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
                "demo.plan": {
                    "intent": "demo.plan",
                    "description": "demo plan",
                    "runbook": "demo.plan",
                    "tags": ["demo"],
                    "inputs": { "required": [], "defaults": {}, "map": {} },
                    "when": {},
                    "effects": { "kind": "read", "requires_apply": false }
                },
                "demo.prepare": {
                    "intent": "demo.prepare",
                    "description": "demo prepare",
                    "runbook": "demo.prepare",
                    "tags": ["demo"],
                    "inputs": { "required": [], "defaults": {}, "map": {} },
                    "when": {},
                    "effects": { "kind": "write", "requires_apply": true }
                },
                "demo.sync": {
                    "intent": "demo.sync",
                    "description": "demo apply",
                    "runbook": "demo.sync",
                    "depends_on": ["demo.prepare"],
                    "tags": ["demo"],
                    "inputs": { "required": [], "defaults": {}, "map": {} },
                    "when": {},
                    "effects": { "kind": "write", "requires_apply": true }
                }
            }
        }),
    );

    set_env("INFRA_PROFILES_DIR", &tmp_dir);
    set_env("INFRA_STORE_DB_PATH", &tmp_dir.join("infra.db"));
    set_env("INFRA_DEFAULT_RUNBOOKS_PATH", &runbooks_path);
    set_env("INFRA_DEFAULT_CAPABILITIES_PATH", &capabilities_path);
    std::env::remove_var("INFRA_CAPABILITIES_PATH");

    let logger = Logger::new("test");
    let validation = Validation::new();
    let security = Arc::new(Security::new().expect("security"));
    let state_service = Arc::new(StateService::new().expect("state"));
    let context_service = Arc::new(ContextService::new().expect("context"));
    let capability_service =
        Arc::new(CapabilityService::new(security.clone()).expect("capability service"));
    let runbook_service = Arc::new(RunbookService::new().expect("runbook service"));
    let evidence_service = Arc::new(EvidenceService::new(
        logger.clone(),
        security.as_ref().clone(),
    ));
    let operation_service = Arc::new(OperationService::new().expect("operation service"));
    let job_service = Arc::new(JobService::new(logger.clone()).expect("job service"));

    let intent_manager = Arc::new(IntentManager::new(
        logger.clone(),
        security.clone(),
        validation.clone(),
        capability_service.clone(),
        runbook_service.clone(),
        evidence_service,
        state_service.clone(),
        None,
        Some(context_service.clone()),
        None,
    ));

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
        state_service,
        None,
        None,
        handlers,
        HashMap::new(),
    ));
    intent_manager.set_tool_executor(tool_executor.clone());

    let operation_manager = OperationManager::new(
        logger.clone(),
        validation,
        capability_service,
        runbook_service.clone(),
        Some(context_service),
        intent_manager,
        operation_service.clone(),
        job_service,
    );

    let planned = operation_manager
        .handle_action(serde_json::json!({
            "action": "plan",
            "family": "demo",
        }))
        .await
        .expect("plan operation");
    assert_eq!(
        planned
            .get("operation")
            .and_then(|v| v.get("status"))
            .and_then(|v| v.as_str()),
        Some("planned")
    );
    assert_eq!(
        planned
            .get("operation")
            .and_then(|v| v.get("capability"))
            .and_then(|v| v.as_str()),
        Some("demo.plan")
    );
    assert!(planned
        .get("operation")
        .and_then(|v| v.get("trace_id"))
        .and_then(|v| v.as_str())
        .map(|value| !value.is_empty())
        .unwrap_or(false));
    assert!(planned
        .get("operation")
        .and_then(|v| v.get("span_id"))
        .and_then(|v| v.as_str())
        .map(|value| !value.is_empty())
        .unwrap_or(false));
    assert_eq!(
        planned
            .get("resolved_capability")
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert_eq!(
        planned
            .get("operation")
            .and_then(|v| v.get("capability_manifest"))
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert_eq!(
        planned
            .get("resolved_capability")
            .and_then(|v| v.get("manifest_version")),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        planned
            .get("resolved_runbook_manifest")
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert_eq!(
        planned
            .get("operation")
            .and_then(|v| v.get("runbook_manifest"))
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str()),
        Some("demo.plan")
    );
    assert_eq!(
        planned
            .get("operation")
            .and_then(|v| v.get("runbook_manifest"))
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );

    let operation_id = planned
        .get("operation")
        .and_then(|v| v.get("operation_id"))
        .and_then(|v| v.as_str())
        .expect("operation id")
        .to_string();
    let status = operation_manager
        .handle_action(serde_json::json!({
            "action": "status",
            "operation_id": operation_id,
        }))
        .await
        .expect("status");
    assert_eq!(
        status
            .get("operation")
            .and_then(|v| v.get("capability"))
            .and_then(|v| v.as_str()),
        Some("demo.plan")
    );
    assert!(status
        .get("operation")
        .and_then(|v| v.get("trace_id"))
        .and_then(|v| v.as_str())
        .map(|value| !value.is_empty())
        .unwrap_or(false));

    let applied = operation_manager
        .handle_action(serde_json::json!({
            "action": "apply",
            "family": "demo",
            "apply": true,
        }))
        .await
        .expect("apply operation");
    assert!(applied
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false));
    assert_eq!(
        applied
            .get("operation")
            .and_then(|v| v.get("status"))
            .and_then(|v| v.as_str()),
        Some("completed")
    );
    assert_eq!(
        applied
            .get("operation")
            .and_then(|v| v.get("capability"))
            .and_then(|v| v.as_str()),
        Some("demo.sync")
    );
    let applied_operation_id = applied
        .get("operation")
        .and_then(|v| v.get("operation_id"))
        .and_then(|v| v.as_str())
        .expect("applied operation id")
        .to_string();
    assert_eq!(
        applied
            .get("resolved_capability")
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert!(applied
        .get("resolved_capability")
        .and_then(|v| v.get("manifest_sha256"))
        .and_then(|v| v.as_str())
        .map(|value| value.len() == 64)
        .unwrap_or(false));
    assert_eq!(
        applied
            .get("resolved_runbook_manifest")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str()),
        Some("demo.sync")
    );
    assert_eq!(
        applied
            .get("resolved_runbook_manifest")
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert_eq!(
        applied
            .get("operation")
            .and_then(|v| v.get("runbook_manifest"))
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str()),
        Some("demo.sync")
    );
    assert_eq!(
        applied
            .get("operation")
            .and_then(|v| v.get("runbook_manifest"))
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert_eq!(
        applied
            .get("operation")
            .and_then(|v| v.get("runbook_manifests"))
            .and_then(|v| v.as_array())
            .map(|items| items.len()),
        Some(2)
    );
    assert!(applied
        .get("operation")
        .and_then(|v| v.get("description_snapshot"))
        .and_then(|v| v.get("hash"))
        .and_then(|v| v.as_str())
        .map(|value| !value.is_empty())
        .unwrap_or(false));
    assert_eq!(
        applied
            .get("operation")
            .and_then(|v| v.get("capability_manifest"))
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert!(applied
        .get("operation")
        .and_then(|v| v.get("trace_id"))
        .and_then(|v| v.as_str())
        .map(|value| !value.is_empty())
        .unwrap_or(false));
    assert!(applied
        .get("operation")
        .and_then(|v| v.get("span_id"))
        .and_then(|v| v.as_str())
        .map(|value| !value.is_empty())
        .unwrap_or(false));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let operations = operation_service.list(10, None).expect("operations");
    assert_eq!(operations.len(), 2);
    let reloaded_operation_service = OperationService::new().expect("reloaded operation service");
    let persisted = reloaded_operation_service
        .get(&applied_operation_id)
        .expect("persisted receipt lookup")
        .expect("persisted receipt");
    assert_eq!(
        persisted
            .get("capability_manifest")
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert_eq!(
        persisted
            .get("runbook_manifest")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str()),
        Some("demo.sync")
    );
    assert_eq!(
        persisted
            .get("runbook_manifests")
            .and_then(|v| v.as_array())
            .map(|items| items.len()),
        Some(2)
    );
    assert!(persisted
        .get("trace_id")
        .and_then(|v| v.as_str())
        .map(|value| !value.is_empty())
        .unwrap_or(false));
    assert!(persisted
        .get("span_id")
        .and_then(|v| v.as_str())
        .map(|value| !value.is_empty())
        .unwrap_or(false));
    assert!(Arc::strong_count(&tool_executor) >= 1);

    restore_env("INFRA_CAPABILITIES_PATH", prev_capabilities);
    restore_env("INFRA_STORE_DB_PATH", prev_store_db);
    restore_env("INFRA_DEFAULT_CAPABILITIES_PATH", prev_default_capabilities);
    restore_env("INFRA_DEFAULT_RUNBOOKS_PATH", prev_default_runbooks);
    restore_env("INFRA_PROFILES_DIR", prev_profiles);
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[tokio::test]
async fn operation_plan_prefers_project_capability_manifest_over_default_manifest() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();
    let prev_capabilities = std::env::var("INFRA_CAPABILITIES_PATH").ok();
    let prev_default_capabilities = std::env::var("INFRA_DEFAULT_CAPABILITIES_PATH").ok();
    let prev_default_runbooks = std::env::var("INFRA_DEFAULT_RUNBOOKS_PATH").ok();
    let prev_store_db = std::env::var("INFRA_STORE_DB_PATH").ok();

    let tmp_dir =
        std::env::temp_dir().join(format!("infra-operation-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let runbooks_path = tmp_dir.join("runbooks.json");
    write_json(
        &runbooks_path,
        &serde_json::json!({
            "demo.plan": {
                "effects": { "kind": "read", "requires_apply": false, "irreversible": false },
                "steps": [
                    { "tool": "dummy", "args": { "action": "preview" } }
                ]
            }
        }),
    );

    let default_capabilities_path = tmp_dir.join("defaults-capabilities.json");
    write_json(
        &default_capabilities_path,
        &serde_json::json!({
            "version": 1,
            "capabilities": {
                "demo.plan": {
                    "intent": "demo.plan",
                    "description": "default demo plan",
                    "runbook": "demo.plan",
                    "tags": ["default"],
                    "inputs": { "required": [], "defaults": {}, "map": {} },
                    "when": {},
                    "effects": { "kind": "read", "requires_apply": false }
                }
            }
        }),
    );

    let project_capabilities_path = tmp_dir.join("capabilities.json");
    write_json(
        &project_capabilities_path,
        &serde_json::json!({
            "version": 2,
            "capabilities": {
                "demo.plan": {
                    "intent": "demo.plan",
                    "description": "project demo plan",
                    "runbook": "demo.plan",
                    "tags": ["project"],
                    "inputs": { "required": [], "defaults": {}, "map": {} },
                    "when": {},
                    "effects": { "kind": "read", "requires_apply": false }
                }
            }
        }),
    );

    set_env("INFRA_PROFILES_DIR", &tmp_dir);
    set_env("INFRA_STORE_DB_PATH", &tmp_dir.join("infra.db"));
    set_env("INFRA_DEFAULT_RUNBOOKS_PATH", &runbooks_path);
    set_env(
        "INFRA_DEFAULT_CAPABILITIES_PATH",
        &default_capabilities_path,
    );
    set_env("INFRA_CAPABILITIES_PATH", &project_capabilities_path);

    let logger = Logger::new("test");
    let validation = Validation::new();
    let security = Arc::new(Security::new().expect("security"));
    let state_service = Arc::new(StateService::new().expect("state"));
    let context_service = Arc::new(ContextService::new().expect("context"));
    let capability_service =
        Arc::new(CapabilityService::new(security.clone()).expect("capability service"));
    let runbook_service = Arc::new(RunbookService::new().expect("runbook service"));
    let evidence_service = Arc::new(EvidenceService::new(
        logger.clone(),
        security.as_ref().clone(),
    ));
    let operation_service = Arc::new(OperationService::new().expect("operation service"));
    let job_service = Arc::new(JobService::new(logger.clone()).expect("job service"));

    let intent_manager = Arc::new(IntentManager::new(
        logger.clone(),
        security,
        validation.clone(),
        capability_service.clone(),
        runbook_service.clone(),
        evidence_service,
        state_service.clone(),
        None,
        Some(context_service.clone()),
        None,
    ));

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
        state_service,
        None,
        None,
        handlers,
        HashMap::new(),
    ));
    intent_manager.set_tool_executor(tool_executor);

    let operation_manager = OperationManager::new(
        logger,
        validation,
        capability_service,
        runbook_service.clone(),
        Some(context_service),
        intent_manager,
        operation_service,
        job_service,
    );

    let planned = operation_manager
        .handle_action(serde_json::json!({
            "action": "plan",
            "family": "demo",
        }))
        .await
        .expect("plan operation");
    assert_eq!(
        planned
            .get("resolved_capability")
            .and_then(|v| v.get("description"))
            .and_then(|v| v.as_str()),
        Some("project demo plan")
    );
    assert_eq!(
        planned
            .get("resolved_capability")
            .and_then(|v| v.get("source"))
            .and_then(|v| v.as_str()),
        Some("manifest")
    );
    assert_eq!(
        planned
            .get("resolved_capability")
            .and_then(|v| v.get("manifest_path"))
            .and_then(|v| v.as_str()),
        Some(project_capabilities_path.to_string_lossy().as_ref())
    );
    assert_eq!(
        planned
            .get("operation")
            .and_then(|v| v.get("capability_manifest"))
            .and_then(|v| v.get("manifest_path"))
            .and_then(|v| v.as_str()),
        Some(project_capabilities_path.to_string_lossy().as_ref())
    );
    let operation_id = planned
        .get("operation")
        .and_then(|v| v.get("operation_id"))
        .and_then(|v| v.as_str())
        .expect("planned operation id")
        .to_string();
    let status = operation_manager
        .handle_action(serde_json::json!({
            "action": "status",
            "operation_id": operation_id,
        }))
        .await
        .expect("operation status");
    assert_eq!(
        status
            .get("operation")
            .and_then(|v| v.get("capability_manifest"))
            .and_then(|v| v.get("manifest_path"))
            .and_then(|v| v.as_str()),
        Some(project_capabilities_path.to_string_lossy().as_ref())
    );

    restore_env("INFRA_DEFAULT_RUNBOOKS_PATH", prev_default_runbooks);
    restore_env("INFRA_DEFAULT_CAPABILITIES_PATH", prev_default_capabilities);
    restore_env("INFRA_CAPABILITIES_PATH", prev_capabilities);
    restore_env("INFRA_STORE_DB_PATH", prev_store_db);
    restore_env("INFRA_PROFILES_DIR", prev_profiles);
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[tokio::test]
async fn operation_status_stays_waiting_external_until_job_completes() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();
    let prev_store_db = std::env::var("INFRA_STORE_DB_PATH").ok();

    let tmp_dir =
        std::env::temp_dir().join(format!("infra-operation-status-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    set_env("INFRA_PROFILES_DIR", &tmp_dir);
    set_env("INFRA_STORE_DB_PATH", &tmp_dir.join("infra.db"));

    let logger = Logger::new("test");
    let validation = Validation::new();
    let security = Arc::new(Security::new().expect("security"));
    let state_service = Arc::new(StateService::new().expect("state"));
    let context_service = Arc::new(ContextService::new().expect("context"));
    let capability_service =
        Arc::new(CapabilityService::new(security.clone()).expect("capability service"));
    let runbook_service = Arc::new(RunbookService::new().expect("runbook service"));
    let evidence_service = Arc::new(EvidenceService::new(
        logger.clone(),
        security.as_ref().clone(),
    ));
    let operation_service = Arc::new(OperationService::new().expect("operation service"));
    let job_service = Arc::new(JobService::new(logger.clone()).expect("job service"));
    let intent_manager = Arc::new(IntentManager::new(
        logger.clone(),
        security,
        validation.clone(),
        capability_service.clone(),
        runbook_service.clone(),
        evidence_service,
        state_service,
        None,
        Some(context_service.clone()),
        None,
    ));
    let operation_manager = OperationManager::new(
        logger.clone(),
        validation,
        capability_service,
        runbook_service,
        Some(context_service),
        intent_manager,
        operation_service.clone(),
        job_service.clone(),
    );

    operation_service
        .upsert(
            "op-job",
            &serde_json::json!({
                "operation_id": "op-job",
                "status": "completed",
                "success": true,
                "action": "apply",
                "job_ids": ["job-1"],
                "updated_at": "2026-04-05T10:00:00Z",
                "effects": { "kind": "write", "requires_apply": true, "irreversible": false }
            }),
        )
        .expect("persist operation");
    let _ = job_service.upsert(serde_json::json!({
        "job_id": "job-1",
        "status": "running",
        "started_at": "2026-04-05T10:00:01Z",
        "updated_at": "2026-04-05T10:00:02Z",
        "artifacts": [{ "kind": "log", "path": "/tmp/job-1.log" }]
    }));

    let waiting = operation_manager
        .handle_action(serde_json::json!({
            "action": "status",
            "operation_id": "op-job"
        }))
        .await
        .expect("waiting status");
    assert_eq!(
        waiting
            .pointer("/operation/status")
            .and_then(|v| v.as_str()),
        Some("waiting_external")
    );
    assert_eq!(
        waiting
            .pointer("/operation/success")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        waiting
            .pointer("/operation/job_outcomes/0/status")
            .and_then(|v| v.as_str()),
        Some("running")
    );

    let _ = job_service.upsert(serde_json::json!({
        "job_id": "job-1",
        "status": "succeeded",
        "started_at": "2026-04-05T10:00:01Z",
        "ended_at": "2026-04-05T10:00:03Z",
        "updated_at": "2026-04-05T10:00:03Z",
        "artifacts": [{ "kind": "log", "path": "/tmp/job-1.log" }]
    }));
    let completed = operation_manager
        .handle_action(serde_json::json!({
            "action": "status",
            "operation_id": "op-job"
        }))
        .await
        .expect("completed status");
    assert_eq!(
        completed
            .pointer("/operation/status")
            .and_then(|v| v.as_str()),
        Some("completed")
    );
    assert_eq!(
        completed
            .pointer("/operation/success")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        completed
            .pointer("/operation/job_outcomes/0/status")
            .and_then(|v| v.as_str()),
        Some("completed")
    );

    restore_env("INFRA_STORE_DB_PATH", prev_store_db);
    restore_env("INFRA_PROFILES_DIR", prev_profiles);
    std::fs::remove_dir_all(&tmp_dir).ok();
}
