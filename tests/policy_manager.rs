use infra::services::logger::Logger;
use infra::services::policy::PolicyService;
use serde_json::json;
use std::sync::Arc;

mod common;
use common::ENV_LOCK;

pub mod errors {
    pub use infra::errors::*;
}

pub mod services {
    pub mod logger {
        pub use infra::services::logger::*;
    }

    pub mod policy {
        pub use infra::services::policy::*;
    }

    pub mod tool_executor {
        pub use infra::services::tool_executor::*;
    }
}

pub mod utils {
    pub mod tool_errors {
        pub use infra::utils::tool_errors::*;
    }
}

#[path = "../src/managers/policy.rs"]
mod policy_manager_impl;

use policy_manager_impl::PolicyManager;

fn restore_env(key: &str, previous: Option<String>) {
    match previous {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

#[tokio::test]
async fn policy_manager_resolve_returns_canonical_policy_from_profile() {
    let _guard = ENV_LOCK.lock().await;

    let manager = PolicyManager::new(
        Logger::new("test"),
        Arc::new(PolicyService::new(Logger::new("test"), None)),
    );

    let result = manager
        .handle_action(json!({
            "action": "resolve",
            "inputs": {
                "policy_profile_name": "operatorless"
            },
            "project_context": {
                "project": {
                    "policy_profiles": {
                        "operatorless": {
                            "mode": "operatorless",
                            "allow": {
                                "merge": false
                            },
                            "lock": {
                                "enabled": false
                            }
                        }
                    }
                }
            }
        }))
        .await
        .expect("resolve policy");

    assert_eq!(result.get("success").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        result.get("resolved_from").and_then(|v| v.as_str()),
        Some("inputs.policy_profile_name:operatorless")
    );
    assert_eq!(
        result.pointer("/policy/mode").and_then(|v| v.as_str()),
        Some("operatorless")
    );
    assert_eq!(
        result
            .pointer("/policy/allow/merge")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        result
            .pointer("/policy/lock/enabled")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        result
            .pointer("/policy/lock/ttl_ms")
            .and_then(|v| v.as_i64()),
        Some(15 * 60_000)
    );
}

#[tokio::test]
async fn policy_manager_resolve_uses_env_autonomy_fallback() {
    let _guard = ENV_LOCK.lock().await;

    let prev_policy = std::env::var("INFRA_AUTONOMY_POLICY").ok();
    let prev_autonomy = std::env::var("INFRA_AUTONOMY").ok();
    std::env::set_var("INFRA_AUTONOMY_POLICY", "operatorless");
    std::env::remove_var("INFRA_AUTONOMY");

    let manager = PolicyManager::new(
        Logger::new("test"),
        Arc::new(PolicyService::new(Logger::new("test"), None)),
    );

    let result = manager
        .handle_action(json!({
            "action": "resolve"
        }))
        .await
        .expect("resolve from env");

    assert_eq!(result.get("success").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        result.get("resolved_from").and_then(|v| v.as_str()),
        Some("env.INFRA_AUTONOMY_POLICY")
    );
    assert_eq!(
        result.pointer("/policy/mode").and_then(|v| v.as_str()),
        Some("operatorless")
    );

    restore_env("INFRA_AUTONOMY_POLICY", prev_policy);
    restore_env("INFRA_AUTONOMY", prev_autonomy);
}

#[tokio::test]
async fn policy_manager_evaluate_reports_denied_gitops_writes() {
    let _guard = ENV_LOCK.lock().await;

    let manager = PolicyManager::new(
        Logger::new("test"),
        Arc::new(PolicyService::new(Logger::new("test"), None)),
    );

    let result = manager
        .handle_action(json!({
            "action": "evaluate",
            "intent": "gitops.release",
            "inputs": {
                "policy_profile_name": "operatorless",
                "merge": true
            },
            "project_context": {
                "project": {
                    "policy_profiles": {
                        "operatorless": {
                            "mode": "operatorless",
                            "allow": {
                                "merge": false
                            }
                        }
                    }
                }
            }
        }))
        .await
        .expect("evaluate policy");

    assert_eq!(result.get("success").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(result.get("allowed").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        result.pointer("/evaluation/kind").and_then(|v| v.as_str()),
        Some("gitops_write")
    );
    assert_eq!(
        result
            .pointer("/evaluation/denial/message")
            .and_then(|v| v.as_str()),
        Some("policy denies merge")
    );
}

#[tokio::test]
async fn policy_manager_evaluate_reports_allowed_repo_writes() {
    let _guard = ENV_LOCK.lock().await;

    let manager = PolicyManager::new(
        Logger::new("test"),
        Arc::new(PolicyService::new(Logger::new("test"), None)),
    );

    let result = manager
        .handle_action(json!({
            "action": "evaluate",
            "input": {
                "action": "apply_patch",
                "repo_root": "/repo",
                "policy": {
                    "mode": "operatorless",
                    "repo": {
                        "allowed_remotes": ["origin"]
                    },
                    "lock": {
                        "enabled": false
                    }
                },
                "remote": "origin"
            }
        }))
        .await
        .expect("evaluate repo policy");

    assert_eq!(result.get("success").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(result.get("allowed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        result.pointer("/evaluation/kind").and_then(|v| v.as_str()),
        Some("repo_write")
    );
    assert!(result.pointer("/evaluation/denial").is_some());
    assert_eq!(
        result.pointer("/evaluation/denial"),
        Some(&serde_json::Value::Null)
    );
}
