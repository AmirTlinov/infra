use infra::services::context::ContextService;

mod common;
use common::ENV_LOCK;

fn restore_env(key: &str, previous: Option<String>) {
    match previous {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

#[tokio::test]
async fn context_reads_are_recomputed_without_session_cache() {
    let _guard = ENV_LOCK.lock().await;

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();
    let tmp_dir = std::env::temp_dir().join(format!("infra-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    std::fs::write(
        tmp_dir.join("Cargo.toml"),
        "[package]\nname = \"tmp\"\nversion = \"0.1.0\"\n",
    )
    .expect("write marker");
    std::env::set_var("INFRA_PROFILES_DIR", &tmp_dir);

    let args = serde_json::json!({
        "cwd": tmp_dir,
        "key": "context-derived",
    });

    let first_service = ContextService::new().expect("context service");
    let first = first_service
        .get_context(&args)
        .await
        .expect("context result");

    assert!(first
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false));
    assert_eq!(
        first
            .get("context")
            .and_then(|v| v.get("signals"))
            .and_then(|v| v.get("rust"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert!(
        !tmp_dir.join("context.json").exists(),
        "derived context reads must not persist a context.json file"
    );

    std::fs::remove_file(tmp_dir.join("Cargo.toml")).expect("remove marker");

    let refreshed_without_flag = first_service
        .get_context(&args)
        .await
        .expect("recomputed context result");
    assert_eq!(
        refreshed_without_flag
            .get("context")
            .and_then(|v| v.get("signals"))
            .and_then(|v| v.get("rust"))
            .and_then(|v| v.as_bool()),
        Some(false),
        "same service instance must see the new filesystem state immediately"
    );

    let refreshed = first_service
        .get_context(&serde_json::json!({
            "cwd": tmp_dir,
            "key": "context-derived",
            "refresh": true,
        }))
        .await
        .expect("refreshed context result");
    assert_eq!(
        refreshed
            .get("context")
            .and_then(|v| v.get("signals"))
            .and_then(|v| v.get("rust"))
            .and_then(|v| v.as_bool()),
        Some(false),
        "explicit refresh remains equivalent to the default no-cache behavior"
    );

    let second_service = ContextService::new().expect("second context service");
    let second = second_service
        .get_context(&args)
        .await
        .expect("second context result");
    assert_eq!(
        second
            .get("context")
            .and_then(|v| v.get("signals"))
            .and_then(|v| v.get("rust"))
            .and_then(|v| v.as_bool()),
        Some(false),
        "derived context must not persist across service instances or restarts"
    );

    restore_env("INFRA_PROFILES_DIR", prev_profiles);
}
