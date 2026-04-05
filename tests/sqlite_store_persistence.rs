use infra::services::job::JobService;
use infra::services::logger::Logger;
use infra::services::runbook::RunbookService;
use infra::services::state::StateService;
use serde_json::Value;

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
    std::fs::write(path, format!("{}\n", payload)).expect("write file");
}

#[tokio::test]
async fn sqlite_persistent_state_preserves_concurrent_service_writes() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();
    let tmp_dir = std::env::temp_dir().join(format!("infra-sqlite-state-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    std::env::set_var("INFRA_PROFILES_DIR", &tmp_dir);

    let service_a = StateService::new().expect("state a");
    let service_b = StateService::new().expect("state b");

    service_a
        .set("alpha", serde_json::json!(1), Some("persistent"))
        .expect("set alpha");
    service_b
        .set("beta", serde_json::json!(2), Some("persistent"))
        .expect("set beta");

    let service_c = StateService::new().expect("state c");
    let dump = service_c.dump(Some("persistent")).expect("dump");
    let state = dump
        .get("state")
        .and_then(|v| v.as_object())
        .expect("persistent object");

    assert_eq!(state.get("alpha"), Some(&serde_json::json!(1)));
    assert_eq!(state.get("beta"), Some(&serde_json::json!(2)));
    assert!(
        tmp_dir.join("infra.db").exists(),
        "sqlite store should be created"
    );

    restore_env("INFRA_PROFILES_DIR", prev_profiles);
}

#[tokio::test]
async fn sqlite_jobs_persist_across_service_instances_without_jobs_json_writes() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();
    let tmp_dir = std::env::temp_dir().join(format!("infra-sqlite-jobs-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    std::env::set_var("INFRA_PROFILES_DIR", &tmp_dir);

    let logger = Logger::new("test");
    let service_a = JobService::new(logger.clone()).expect("job service a");
    let created = service_a.create(serde_json::json!({
        "kind": "ssh_exec",
        "status": "queued",
        "progress": {"step": "enqueue"}
    }));
    let job_id = created
        .get("job_id")
        .and_then(|v| v.as_str())
        .expect("job id")
        .to_string();

    let service_b = JobService::new(logger).expect("job service b");
    let loaded = service_b.get(&job_id).expect("persisted job");
    assert_eq!(
        loaded.get("kind").and_then(|v| v.as_str()),
        Some("ssh_exec")
    );
    assert_eq!(
        loaded
            .get("progress")
            .and_then(|v| v.get("step"))
            .and_then(|v| v.as_str()),
        Some("enqueue")
    );
    assert!(
        tmp_dir.join("infra.db").exists(),
        "jobs should persist in infra.db"
    );
    assert!(
        !tmp_dir.join("jobs.json").exists(),
        "creating jobs must not rewrite a legacy jobs.json store"
    );

    restore_env("INFRA_PROFILES_DIR", prev_profiles);
}

#[tokio::test]
async fn sqlite_jobs_import_legacy_jobs_json_once() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();
    let tmp_dir = std::env::temp_dir().join(format!("infra-legacy-jobs-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    std::env::set_var("INFRA_PROFILES_DIR", &tmp_dir);

    write_json(
        &tmp_dir.join("jobs.json"),
        &serde_json::json!({
            "version": 1,
            "jobs": [
                {
                    "job_id": "legacy-job-1",
                    "kind": "ssh_exec",
                    "status": "succeeded",
                    "created_at": "2026-03-06T00:00:00Z",
                    "updated_at": "2026-03-06T00:00:00Z",
                    "ended_at": "2026-03-06T00:00:00Z",
                    "expires_at_ms": 4102444800000i64
                }
            ]
        }),
    );

    let logger = Logger::new("test");
    let service_a = JobService::new(logger.clone()).expect("job service a");
    let imported = service_a.get("legacy-job-1").expect("imported job");
    assert_eq!(
        imported.get("status").and_then(|v| v.as_str()),
        Some("succeeded")
    );

    std::fs::remove_file(tmp_dir.join("jobs.json")).expect("remove legacy jobs file");

    let service_b = JobService::new(logger).expect("job service b");
    let imported_again = service_b.get("legacy-job-1").expect("job still in sqlite");
    assert_eq!(
        imported_again.get("kind").and_then(|v| v.as_str()),
        Some("ssh_exec")
    );

    restore_env("INFRA_PROFILES_DIR", prev_profiles);
}

#[tokio::test]
async fn runbook_manifests_prefer_project_entries_over_defaults_after_restart() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();
    let prev_runbook_manifest = std::env::var("INFRA_RUNBOOKS_PATH").ok();
    let prev_runbooks = std::env::var("INFRA_DEFAULT_RUNBOOKS_PATH").ok();

    let tmp_dir =
        std::env::temp_dir().join(format!("infra-sqlite-runbook-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    std::env::set_var("INFRA_PROFILES_DIR", &tmp_dir);

    let defaults_path = tmp_dir.join("defaults-runbooks.json");
    write_json(
        &defaults_path,
        &serde_json::json!({
            "default.echo": {
                "description": "default runbook",
                "steps": [
                    { "tool": "help", "args": {} }
                ]
            },
            "shared.echo": {
                "description": "default shared runbook",
                "steps": [
                    { "tool": "help", "args": { "query": "default" } }
                ]
            }
        }),
    );
    std::env::set_var("INFRA_DEFAULT_RUNBOOKS_PATH", &defaults_path);

    let project_manifest_path = tmp_dir.join("runbooks.json");
    std::env::set_var("INFRA_RUNBOOKS_PATH", &project_manifest_path);

    write_json(
        &project_manifest_path,
        &serde_json::json!({
            "shared.echo": {
                "description": "project shared runbook",
                "steps": [
                    { "tool": "help", "args": { "query": "project" } }
                ]
            },
            "project.only": {
                "description": "project only runbook",
                "steps": [
                    { "tool": "help", "args": { "query": "project-only" } }
                ]
            }
        }),
    );

    let service_a = RunbookService::new().expect("runbook service a");
    let listed = service_a
        .list_runbooks(&infra::utils::listing::ListFilters::default())
        .expect("list manifests");
    assert_eq!(
        listed
            .get("meta")
            .and_then(|v| v.get("total"))
            .and_then(|v| v.as_u64()),
        Some(3)
    );

    let shared = service_a
        .get_runbook("shared.echo")
        .expect("project override should resolve");
    assert_eq!(
        shared
            .get("runbook")
            .and_then(|v| v.get("description"))
            .and_then(|v| v.as_str()),
        Some("project shared runbook")
    );
    assert_eq!(
        shared
            .get("runbook")
            .and_then(|v| v.get("source"))
            .and_then(|v| v.as_str()),
        Some("manifest")
    );
    assert_eq!(
        shared
            .get("runbook")
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );
    assert_eq!(
        shared
            .get("runbook")
            .and_then(|v| v.get("manifest_path"))
            .and_then(|v| v.as_str()),
        Some(project_manifest_path.to_string_lossy().as_ref())
    );
    assert!(shared
        .get("runbook")
        .and_then(|v| v.get("manifest_sha256"))
        .and_then(|v| v.as_str())
        .map(|value| value.len() == 64)
        .unwrap_or(false));

    let service_b = RunbookService::new().expect("runbook service b");
    let project_only = service_b
        .get_runbook("project.only")
        .expect("project manifest should still resolve after restart");
    assert_eq!(
        project_only
            .get("runbook")
            .and_then(|v| v.get("description"))
            .and_then(|v| v.as_str()),
        Some("project only runbook")
    );
    assert_eq!(
        project_only
            .get("runbook")
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("file_backed_manifest")
    );

    restore_env("INFRA_DEFAULT_RUNBOOKS_PATH", prev_runbooks);
    restore_env("INFRA_RUNBOOKS_PATH", prev_runbook_manifest);
    restore_env("INFRA_PROFILES_DIR", prev_profiles);
}
