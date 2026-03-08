use infra::errors::ToolErrorKind;
use infra::managers::capability::CapabilityManager;
use infra::services::capability::CapabilityService;
use infra::services::logger::Logger;
use infra::services::security::Security;
use infra::services::store_db::StoreDb;
use infra::services::validation::Validation;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

mod common;
use common::ENV_LOCK;

fn restore_env(key: &str, previous: Option<String>) {
    match previous {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

fn write_json(path: &std::path::Path, value: &Value) {
    let payload = serde_json::to_string_pretty(value).expect("serialize json");
    std::fs::write(path, format!("{}\n", payload)).expect("write json");
}

fn manifest_sha256(value: &Value) -> String {
    let payload = serde_json::to_string_pretty(value).expect("serialize manifest") + "\n";
    format!("{:x}", Sha256::digest(payload.as_bytes()))
}

#[tokio::test]
async fn capability_reads_are_manifest_first_and_expose_provenance() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
    let prev_defaults = std::env::var("MCP_DEFAULT_CAPABILITIES_PATH").ok();
    let prev_capabilities = std::env::var("MCP_CAPABILITIES_PATH").ok();

    let tmp_dir =
        std::env::temp_dir().join(format!("infra-capability-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let manifest_path = tmp_dir.join("default-capabilities.json");
    let manifest = json!({
        "version": 7,
        "capabilities": {
            "gitops.plan": {
                "intent": "gitops.plan",
                "description": "manifest-backed plan",
                "runbook": "gitops.plan",
                "tags": ["gitops", "read"],
                "inputs": {"required": [], "defaults": {}, "map": {}},
                "when": {},
                "effects": {"kind": "read", "requires_apply": false}
            },
            "gitops.apply": {
                "intent": "gitops.apply",
                "description": "manifest-backed apply",
                "runbook": "gitops.apply",
                "tags": ["gitops", "write"],
                "inputs": {"required": [], "defaults": {}, "map": {}},
                "when": {},
                "effects": {"kind": "write", "requires_apply": true}
            }
        }
    });
    write_json(&manifest_path, &manifest);

    std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);
    std::env::set_var("MCP_DEFAULT_CAPABILITIES_PATH", &manifest_path);
    std::env::set_var("MCP_CAPABILITIES_PATH", &manifest_path);

    let store = StoreDb::new().expect("store db");
    store
        .tombstone("capabilities:local", "gitops.plan", Some("local"))
        .expect("write tombstone");
    store
        .upsert(
            "capabilities:local",
            "shadow.plan",
            &json!({
                "name": "shadow.plan",
                "intent": "shadow.plan",
                "description": "store overlay should be ignored"
            }),
            Some("local"),
        )
        .expect("write overlay record");

    let security = Arc::new(Security::new().expect("security"));
    let service = Arc::new(CapabilityService::new(security).expect("capability service"));
    let manager = CapabilityManager::new(Logger::new("test"), Validation::new(), service, None);

    let expected_sha = manifest_sha256(&manifest);
    let expected_path = manifest_path.to_string_lossy().to_string();

    let listed = manager
        .handle_action(json!({ "action": "list" }))
        .await
        .expect("capability list");
    let names = listed
        .get("capabilities")
        .and_then(|v| v.as_array())
        .expect("capabilities array")
        .iter()
        .filter_map(|value| value.get("name").and_then(|v| v.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["gitops.apply", "gitops.plan"]);
    let listed_plan = listed
        .get("capabilities")
        .and_then(|v| v.as_array())
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("name").and_then(|v| v.as_str()) == Some("gitops.plan"))
        })
        .expect("listed gitops.plan");
    assert_eq!(
        listed_plan.get("description").and_then(|v| v.as_str()),
        Some("manifest-backed plan")
    );
    assert_eq!(
        listed_plan.get("manifest_path").and_then(|v| v.as_str()),
        Some(expected_path.as_str())
    );
    assert_eq!(
        listed_plan.get("manifest_source").and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert_eq!(listed_plan.get("manifest_version"), Some(&json!(7)));
    assert_eq!(
        listed_plan.get("manifest_sha256").and_then(|v| v.as_str()),
        Some(expected_sha.as_str())
    );
    assert_eq!(
        listed
            .get("manifest")
            .and_then(|v| v.get("manifest_version")),
        Some(&json!(7))
    );

    let fetched = manager
        .handle_action(json!({ "action": "get", "name": "gitops.plan" }))
        .await
        .expect("capability get");
    assert_eq!(
        fetched
            .get("capability")
            .and_then(|v| v.get("description"))
            .and_then(|v| v.as_str()),
        Some("manifest-backed plan")
    );
    assert_eq!(
        fetched
            .get("capability")
            .and_then(|v| v.get("manifest_path"))
            .and_then(|v| v.as_str()),
        Some(expected_path.as_str())
    );

    let resolved = manager
        .handle_action(json!({ "action": "resolve", "intent": "gitops.plan" }))
        .await
        .expect("capability resolve");
    assert_eq!(
        resolved
            .get("capability")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str()),
        Some("gitops.plan")
    );
    assert_eq!(
        resolved
            .get("capability")
            .and_then(|v| v.get("manifest_sha256"))
            .and_then(|v| v.as_str()),
        Some(expected_sha.as_str())
    );

    let families = manager
        .handle_action(json!({ "action": "families" }))
        .await
        .expect("capability families");
    let gitops = families
        .get("families")
        .and_then(|v| v.as_array())
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("family").and_then(|v| v.as_str()) == Some("gitops"))
        })
        .expect("gitops family");
    assert_eq!(
        gitops
            .get("capabilities")
            .and_then(|v| v.as_array())
            .map(|items| items.len()),
        Some(2)
    );
    assert_eq!(
        gitops.get("manifest_path").and_then(|v| v.as_str()),
        Some(expected_path.as_str())
    );
    assert_eq!(
        gitops.get("manifest_sha256").and_then(|v| v.as_str()),
        Some(expected_sha.as_str())
    );

    let service_restarted = Arc::new(
        CapabilityService::new(Arc::new(Security::new().expect("security restart")))
            .expect("capability service restart"),
    );
    let manager_restarted = CapabilityManager::new(
        Logger::new("test"),
        Validation::new(),
        service_restarted,
        None,
    );
    let restarted = manager_restarted
        .handle_action(json!({ "action": "get", "name": "gitops.plan" }))
        .await
        .expect("capability get after restart");
    assert_eq!(
        restarted
            .get("capability")
            .and_then(|v| v.get("manifest_path"))
            .and_then(|v| v.as_str()),
        Some(expected_path.as_str())
    );
    assert_eq!(
        restarted
            .get("capability")
            .and_then(|v| v.get("manifest_sha256"))
            .and_then(|v| v.as_str()),
        Some(expected_sha.as_str())
    );

    restore_env("MCP_DEFAULT_CAPABILITIES_PATH", prev_defaults);
    restore_env("MCP_CAPABILITIES_PATH", prev_capabilities);
    restore_env("MCP_PROFILES_DIR", prev_profiles);
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[tokio::test]
async fn capability_manifest_prefers_project_over_default_manifest() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
    let prev_defaults = std::env::var("MCP_DEFAULT_CAPABILITIES_PATH").ok();
    let prev_capabilities = std::env::var("MCP_CAPABILITIES_PATH").ok();

    let tmp_dir =
        std::env::temp_dir().join(format!("infra-capability-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let default_manifest_path = tmp_dir.join("defaults-capabilities.json");
    let default_manifest = json!({
        "version": 1,
        "capabilities": {
            "demo.plan": {
                "intent": "demo.plan",
                "description": "default plan",
                "runbook": "demo.plan",
                "tags": ["default"],
                "inputs": {"required": [], "defaults": {}, "map": {}},
                "when": {},
                "effects": {"kind": "read", "requires_apply": false}
            },
            "demo.verify": {
                "intent": "demo.verify",
                "description": "default verify",
                "runbook": "demo.verify",
                "tags": ["default"],
                "inputs": {"required": [], "defaults": {}, "map": {}},
                "when": {},
                "effects": {"kind": "read", "requires_apply": false}
            }
        }
    });
    write_json(&default_manifest_path, &default_manifest);

    let project_manifest_path = tmp_dir.join("capabilities.json");
    let project_manifest = json!({
        "version": 2,
        "capabilities": {
            "demo.plan": {
                "intent": "demo.plan",
                "description": "project override plan",
                "runbook": "demo.plan",
                "tags": ["project"],
                "inputs": {"required": [], "defaults": {}, "map": {}},
                "when": {},
                "effects": {"kind": "read", "requires_apply": false}
            },
            "demo.apply": {
                "intent": "demo.apply",
                "description": "project apply",
                "runbook": "demo.apply",
                "tags": ["project"],
                "inputs": {"required": [], "defaults": {}, "map": {}},
                "when": {},
                "effects": {"kind": "write", "requires_apply": true}
            }
        }
    });
    write_json(&project_manifest_path, &project_manifest);

    std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);
    std::env::set_var("MCP_DEFAULT_CAPABILITIES_PATH", &default_manifest_path);
    std::env::set_var("MCP_CAPABILITIES_PATH", &project_manifest_path);

    let security = Arc::new(Security::new().expect("security"));
    let service = Arc::new(CapabilityService::new(security).expect("capability service"));
    let manager = CapabilityManager::new(Logger::new("test"), Validation::new(), service, None);

    let expected_project_sha = manifest_sha256(&project_manifest);
    let expected_default_sha = manifest_sha256(&default_manifest);

    let listed = manager
        .handle_action(json!({ "action": "list" }))
        .await
        .expect("capability list");
    let names = listed
        .get("capabilities")
        .and_then(|v| v.as_array())
        .expect("capabilities array")
        .iter()
        .filter_map(|value| value.get("name").and_then(|v| v.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["demo.apply", "demo.plan", "demo.verify"]);

    let listed_plan = listed
        .get("capabilities")
        .and_then(|v| v.as_array())
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("name").and_then(|v| v.as_str()) == Some("demo.plan"))
        })
        .expect("listed demo.plan");
    assert_eq!(
        listed_plan.get("description").and_then(|v| v.as_str()),
        Some("project override plan")
    );
    assert_eq!(
        listed_plan.get("source").and_then(|v| v.as_str()),
        Some("manifest")
    );
    assert_eq!(
        listed_plan.get("manifest_path").and_then(|v| v.as_str()),
        Some(project_manifest_path.to_string_lossy().as_ref())
    );
    assert_eq!(
        listed_plan.get("manifest_sha256").and_then(|v| v.as_str()),
        Some(expected_project_sha.as_str())
    );

    let listed_verify = listed
        .get("capabilities")
        .and_then(|v| v.as_array())
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("name").and_then(|v| v.as_str()) == Some("demo.verify"))
        })
        .expect("listed demo.verify");
    assert_eq!(
        listed_verify.get("description").and_then(|v| v.as_str()),
        Some("default verify")
    );
    assert_eq!(
        listed_verify.get("source").and_then(|v| v.as_str()),
        Some("default_manifest")
    );
    assert_eq!(
        listed_verify.get("manifest_path").and_then(|v| v.as_str()),
        Some(default_manifest_path.to_string_lossy().as_ref())
    );
    assert_eq!(
        listed_verify
            .get("manifest_sha256")
            .and_then(|v| v.as_str()),
        Some(expected_default_sha.as_str())
    );

    let fetched = manager
        .handle_action(json!({ "action": "get", "name": "demo.plan" }))
        .await
        .expect("capability get");
    assert_eq!(
        fetched
            .get("capability")
            .and_then(|v| v.get("description"))
            .and_then(|v| v.as_str()),
        Some("project override plan")
    );
    assert_eq!(
        fetched
            .get("capability")
            .and_then(|v| v.get("manifest_path"))
            .and_then(|v| v.as_str()),
        Some(project_manifest_path.to_string_lossy().as_ref())
    );

    let resolved = manager
        .handle_action(json!({ "action": "resolve", "intent": "demo.plan" }))
        .await
        .expect("capability resolve");
    assert_eq!(
        resolved
            .get("capability")
            .and_then(|v| v.get("manifest_path"))
            .and_then(|v| v.as_str()),
        Some(project_manifest_path.to_string_lossy().as_ref())
    );
    assert_eq!(
        resolved
            .get("capability")
            .and_then(|v| v.get("manifest_sha256"))
            .and_then(|v| v.as_str()),
        Some(expected_project_sha.as_str())
    );

    restore_env("MCP_CAPABILITIES_PATH", prev_capabilities);
    restore_env("MCP_DEFAULT_CAPABILITIES_PATH", prev_defaults);
    restore_env("MCP_PROFILES_DIR", prev_profiles);
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[tokio::test]
async fn capability_mutation_actions_are_compatibility_only() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
    let prev_defaults = std::env::var("MCP_DEFAULT_CAPABILITIES_PATH").ok();
    let prev_capabilities = std::env::var("MCP_CAPABILITIES_PATH").ok();

    let tmp_dir =
        std::env::temp_dir().join(format!("infra-capability-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let manifest_path = tmp_dir.join("default-capabilities.json");
    let manifest = json!({
        "version": 3,
        "capabilities": {
            "gitops.plan": {
                "intent": "gitops.plan",
                "description": "manifest-backed plan",
                "runbook": "gitops.plan",
                "tags": ["gitops"],
                "inputs": {"required": [], "defaults": {}, "map": {}},
                "when": {},
                "effects": {"kind": "read", "requires_apply": false}
            }
        }
    });
    write_json(&manifest_path, &manifest);

    std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);
    std::env::set_var("MCP_DEFAULT_CAPABILITIES_PATH", &manifest_path);
    std::env::set_var("MCP_CAPABILITIES_PATH", &manifest_path);

    let security = Arc::new(Security::new().expect("security"));
    let service = Arc::new(CapabilityService::new(security).expect("capability service"));
    let manager = CapabilityManager::new(Logger::new("test"), Validation::new(), service, None);

    let set_err = manager
        .handle_action(json!({
            "action": "set",
            "name": "gitops.plan",
            "capability": {"description": "mutated"}
        }))
        .await
        .expect_err("set should be rejected");
    assert_eq!(set_err.kind, ToolErrorKind::InvalidParams);
    assert!(set_err.message.contains("compatibility-only"));
    assert_eq!(
        set_err
            .details
            .as_ref()
            .and_then(|v| v.get("stage"))
            .and_then(|v| v.as_str()),
        Some("compatibility_capability_mutation")
    );
    assert_eq!(
        set_err
            .details
            .as_ref()
            .and_then(|v| v.get("manifest_path"))
            .and_then(|v| v.as_str()),
        Some(manifest_path.to_string_lossy().as_ref())
    );

    let delete_err = manager
        .handle_action(json!({
            "action": "delete",
            "name": "gitops.plan"
        }))
        .await
        .expect_err("delete should be rejected");
    assert_eq!(delete_err.kind, ToolErrorKind::InvalidParams);
    assert!(delete_err.message.contains("compatibility-only"));
    assert_eq!(
        delete_err
            .details
            .as_ref()
            .and_then(|v| v.get("action"))
            .and_then(|v| v.as_str()),
        Some("delete")
    );
    assert_eq!(
        delete_err
            .details
            .as_ref()
            .and_then(|v| v.get("manifest_version")),
        Some(&json!(3))
    );

    restore_env("MCP_DEFAULT_CAPABILITIES_PATH", prev_defaults);
    restore_env("MCP_CAPABILITIES_PATH", prev_capabilities);
    restore_env("MCP_PROFILES_DIR", prev_profiles);
    std::fs::remove_dir_all(&tmp_dir).ok();
}
