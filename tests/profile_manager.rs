use infra::managers::profile::ProfileManager;
use infra::services::logger::Logger;
use infra::services::profile::ProfileService;
use infra::services::security::Security;
use std::sync::Arc;

mod common;
use common::ENV_LOCK;

fn restore_env(key: &str, previous: Option<String>) {
    if let Some(value) = previous {
        std::env::set_var(key, value);
    } else {
        std::env::remove_var(key);
    }
}

#[tokio::test]
async fn profile_manager_exposes_read_only_canonical_surface() {
    let _guard = ENV_LOCK.lock().await;

    let tmp_dir = std::env::temp_dir().join(format!("infra-profile-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
    let prev_secret_export = std::env::var("INFRA_ALLOW_SECRET_EXPORT").ok();
    std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);
    std::env::remove_var("INFRA_ALLOW_SECRET_EXPORT");

    let logger = Logger::new("test");
    let security = Arc::new(Security::new().expect("security"));
    let profile_service = Arc::new(ProfileService::new(security).expect("profile service"));
    let manager = ProfileManager::new(logger, profile_service.clone());

    profile_service
        .set_profile(
            "prod-api",
            &serde_json::json!({
                "type": "api",
                "data": {
                    "base_url": "https://example.test",
                    "headers": { "x-team": "infra" }
                },
                "secrets": {
                    "token": "secret-token"
                }
            }),
        )
        .expect("seed profile");

    let listed = manager
        .handle_action(serde_json::json!({
            "action": "list",
            "type": "api"
        }))
        .await
        .expect("list profiles");
    let profiles = listed
        .get("profiles")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(profiles.len(), 1);
    assert_eq!(
        profiles[0].get("name").and_then(|value| value.as_str()),
        Some("prod-api")
    );

    let fetched = manager
        .handle_action(serde_json::json!({
            "action": "get",
            "name": "prod-api"
        }))
        .await
        .expect("get profile");

    assert_eq!(
        fetched
            .pointer("/profile/name")
            .and_then(|value| value.as_str()),
        Some("prod-api")
    );
    assert_eq!(
        fetched
            .pointer("/profile/secrets_redacted")
            .and_then(|value| value.as_bool()),
        Some(true)
    );
    assert_eq!(
        fetched
            .pointer("/profile/secrets/0")
            .and_then(|value| value.as_str()),
        Some("token")
    );
    assert!(fetched.pointer("/profile/secrets/token").is_none());

    let raw = profile_service
        .get_profile("prod-api", Some("api"))
        .expect("profile service get");
    assert_eq!(
        raw.pointer("/secrets/token")
            .and_then(|value| value.as_str()),
        Some("secret-token")
    );

    restore_env("INFRA_ALLOW_SECRET_EXPORT", prev_secret_export);
    restore_env("MCP_PROFILES_DIR", prev_profiles);
    std::fs::remove_dir_all(&tmp_dir).ok();
}
