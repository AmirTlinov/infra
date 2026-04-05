use infra::managers::target::TargetManager;
use infra::services::logger::Logger;
use infra::services::policy::PolicyService;
use infra::services::profile::ProfileService;
use infra::services::project::ProjectService;
use infra::services::security::Security;
use infra::services::state::StateService;
use infra::services::validation::Validation;
use serde_json::json;
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
async fn target_manager_exposes_read_only_resolve_surface() {
    let _guard = ENV_LOCK.lock().await;

    let tmp_dir = std::env::temp_dir().join(format!("infra-target-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let prev_profiles = std::env::var("INFRA_PROFILES_DIR").ok();
    std::env::set_var("INFRA_PROFILES_DIR", &tmp_dir);

    let logger = Logger::new("test");
    let validation = Validation::new();
    let security = Arc::new(Security::new().expect("security"));
    let project_service = Arc::new(ProjectService::new().expect("project service"));
    let state_service = Arc::new(StateService::new().expect("state service"));
    let profile_service = Arc::new(ProfileService::new(security).expect("profile service"));
    let policy_service = Arc::new(PolicyService::new(
        logger.clone(),
        Some(state_service.clone()),
    ));
    profile_service
        .set_profile(
            "prod-ssh",
            &json!({ "type": "ssh", "data": { "host": "prod" } }),
        )
        .expect("seed prod profile");
    profile_service
        .set_profile(
            "staging-ssh",
            &json!({ "type": "ssh", "data": { "host": "staging" } }),
        )
        .expect("seed staging profile");
    let manager = TargetManager::new(
        logger,
        validation,
        project_service.clone(),
        state_service.clone(),
        Some(profile_service.clone()),
        Some(policy_service),
    );

    project_service
        .set_project(
            "demo",
            &json!({
                "description": "Demo project",
                "default_target": "staging",
                "targets": {
                    "prod": {
                        "ssh_profile": "prod-ssh",
                        "cwd": "/srv/prod"
                    },
                    "staging": {
                        "ssh_profile": "staging-ssh",
                        "cwd": "/srv/staging",
                        "policy": {
                            "mode": "operatorless"
                        }
                    }
                }
            }),
        )
        .expect("seed project");

    let listed = manager
        .handle_action(json!({
            "action": "list",
            "project": "demo"
        }))
        .await
        .expect("list targets");
    let targets = listed
        .get("targets")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(targets.len(), 2);
    assert_eq!(
        targets[0].get("name").and_then(|value| value.as_str()),
        Some("prod")
    );
    assert_eq!(
        targets[1].get("name").and_then(|value| value.as_str()),
        Some("staging")
    );
    assert_eq!(
        targets[1].get("default").and_then(|value| value.as_bool()),
        Some(true)
    );

    let fetched = manager
        .handle_action(json!({
            "action": "get",
            "project": "demo",
            "name": "prod"
        }))
        .await
        .expect("get target");
    assert_eq!(
        fetched
            .pointer("/target/target/ssh_profile")
            .and_then(|value| value.as_str()),
        Some("prod-ssh")
    );
    assert_eq!(
        fetched
            .pointer("/target/default")
            .and_then(|value| value.as_bool()),
        Some(false)
    );

    let resolved_default = manager
        .handle_action(json!({
            "action": "resolve",
            "project": "demo"
        }))
        .await
        .expect("resolve default target");
    assert_eq!(
        resolved_default
            .pointer("/target/name")
            .and_then(|value| value.as_str()),
        Some("staging")
    );
    assert_eq!(
        resolved_default
            .pointer("/selection/source")
            .and_then(|value| value.as_str()),
        Some("project_default")
    );
    assert_eq!(
        resolved_default
            .pointer("/resolved/profiles/ssh/name")
            .and_then(|value| value.as_str()),
        Some("staging-ssh")
    );
    assert_eq!(
        resolved_default
            .pointer("/resolved/profiles/ssh/exists")
            .and_then(|value| value.as_bool()),
        Some(true)
    );
    assert_eq!(
        resolved_default
            .pointer("/resolved/paths/cwd/value")
            .and_then(|value| value.as_str()),
        Some("/srv/staging")
    );
    assert_eq!(
        resolved_default
            .pointer("/resolved/policy/policy/mode")
            .and_then(|value| value.as_str()),
        Some("operatorless")
    );

    state_service
        .set("project.active", json!("demo"), Some("session"))
        .expect("set active project");
    state_service
        .set("target.active.demo", json!("prod"), Some("session"))
        .expect("set active target");

    let resolved_active = manager
        .handle_action(json!({
            "action": "resolve",
            "project": "demo"
        }))
        .await
        .expect("resolve active target");
    assert_eq!(
        resolved_active
            .pointer("/target/name")
            .and_then(|value| value.as_str()),
        Some("prod")
    );
    assert_eq!(
        resolved_active
            .pointer("/selection/source")
            .and_then(|value| value.as_str()),
        Some("state")
    );
    assert_eq!(
        resolved_active
            .pointer("/resolved/profiles/ssh/profile/type")
            .and_then(|value| value.as_str()),
        Some("ssh")
    );

    let resolved_explicit = manager
        .handle_action(json!({
            "action": "resolve",
            "project": "demo",
            "name": "staging"
        }))
        .await
        .expect("resolve explicit target");
    assert_eq!(
        resolved_explicit
            .pointer("/target/name")
            .and_then(|value| value.as_str()),
        Some("staging")
    );
    assert_eq!(
        resolved_explicit
            .pointer("/selection/source")
            .and_then(|value| value.as_str()),
        Some("explicit")
    );

    restore_env("INFRA_PROFILES_DIR", prev_profiles);
    std::fs::remove_dir_all(&tmp_dir).ok();
}
