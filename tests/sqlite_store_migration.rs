use infra::errors::ToolErrorKind;
use infra::services::capability::CapabilityService;
use infra::services::security::Security;
use infra::services::state::StateService;
use infra::services::store_db::StoreDb;
use rusqlite::Connection;
use serde_json::json;
use std::sync::Arc;

mod common;
use common::ENV_LOCK;

fn restore_env(key: &str, previous: Option<String>) {
    match previous {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

#[tokio::test]
async fn legacy_state_json_imports_once_into_wal_backed_sqlite_store() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();

    let tmp_dir = std::env::temp_dir().join(format!("infra-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    std::env::set_var("INFRA_PROFILES_DIR", &tmp_dir);

    let state_path = tmp_dir.join("state.json");
    std::fs::write(
        &state_path,
        serde_json::to_string_pretty(&json!({
            "alpha": 1
        }))
        .expect("serialize initial state"),
    )
    .expect("write initial state");

    let service = StateService::new().expect("state service");
    let imported = service.get("alpha", Some("persistent")).expect("get alpha");
    assert_eq!(imported.get("value"), Some(&json!(1)));

    let db_path = tmp_dir.join("infra.db");
    assert!(db_path.exists(), "sqlite store should be created");

    let conn = Connection::open(&db_path).expect("open sqlite db");
    let journal_mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("read journal mode");
    assert_eq!(journal_mode.to_lowercase(), "wal");

    std::fs::write(
        &state_path,
        serde_json::to_string_pretty(&json!({
            "alpha": 99,
            "beta": 2
        }))
        .expect("serialize mutated state"),
    )
    .expect("overwrite legacy state");

    let reloaded = StateService::new().expect("reload state service");
    let alpha = reloaded
        .get("alpha", Some("persistent"))
        .expect("get imported alpha");
    let beta = reloaded
        .get("beta", Some("persistent"))
        .expect("get non-imported beta");
    assert_eq!(alpha.get("value"), Some(&json!(1)));
    assert_eq!(beta.get("value"), Some(&serde_json::Value::Null));

    restore_env("INFRA_PROFILES_DIR", prev_profiles);
}

#[tokio::test]
async fn capability_manifest_ignores_store_tombstones_and_mutation_is_compat_only() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();
    let prev_defaults = std::env::var("INFRA_DEFAULT_CAPABILITIES_PATH").ok();

    let tmp_dir = std::env::temp_dir().join(format!("infra-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    let default_caps = tmp_dir.join("default-capabilities.json");
    std::fs::write(
        &default_caps,
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "capabilities": {
                "gitops.plan": {
                    "intent": "gitops.plan",
                    "runbook": "gitops.plan",
                    "tags": ["gitops"],
                    "inputs": {"required": [], "defaults": {}, "map": {}},
                    "effects": {"kind": "read", "requires_apply": false}
                }
            }
        }))
        .expect("serialize capabilities"),
    )
    .expect("write default capabilities");

    std::env::set_var("INFRA_PROFILES_DIR", &tmp_dir);
    std::env::set_var("INFRA_DEFAULT_CAPABILITIES_PATH", &default_caps);

    let security = Arc::new(Security::new().expect("security"));
    let service = CapabilityService::new(security.clone()).expect("capability service");
    let capability = service
        .get_capability("gitops.plan")
        .expect("default capability visible");
    assert_eq!(
        capability.get("manifest_path").and_then(|v| v.as_str()),
        Some(default_caps.to_string_lossy().as_ref())
    );
    assert_eq!(
        capability.get("manifest_version").and_then(|v| v.as_i64()),
        Some(1)
    );
    assert!(capability
        .get("manifest_sha256")
        .and_then(|v| v.as_str())
        .map(|value| !value.is_empty())
        .unwrap_or(false));
    let store = StoreDb::new().expect("store db");
    store
        .tombstone("capabilities:local", "gitops.plan", Some("local"))
        .expect("write tombstone");

    let reloaded = CapabilityService::new(security.clone()).expect("reload capability service");
    reloaded
        .get_capability("gitops.plan")
        .expect("manifest capability should ignore tombstones");

    let err = service
        .delete_capability("gitops.plan")
        .expect_err("capability mutation should be compat-only");
    assert_eq!(err.kind, ToolErrorKind::InvalidParams);
    assert_eq!(
        err.details
            .as_ref()
            .and_then(|v| v.get("stage"))
            .and_then(|v| v.as_str()),
        Some("compatibility_capability_mutation")
    );

    restore_env("INFRA_DEFAULT_CAPABILITIES_PATH", prev_defaults);
    restore_env("INFRA_PROFILES_DIR", prev_profiles);
}
