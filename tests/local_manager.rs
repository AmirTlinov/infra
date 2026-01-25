mod common;
use common::ENV_LOCK;

use infra::managers::local::LocalManager;
use infra::services::logger::Logger;
use infra::services::validation::Validation;
use serde_json::Value;

fn tmp_dir(prefix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("{}-{}", prefix, uuid::Uuid::new_v4()))
}

#[tokio::test]
async fn local_batch_stop_on_error_default_true() {
    let _guard = ENV_LOCK.lock().await;

    let manager = LocalManager::new(Logger::new("test"), Validation::new(), Some(true));
    let result = manager
        .handle_action(serde_json::json!({
            "action": "batch",
            "commands": [
                { "command": "false" },
                { "command": "echo ok" }
            ]
        }))
        .await
        .expect("batch result");

    assert_eq!(result.get("success").and_then(Value::as_bool), Some(false));
    let results = result
        .get("results")
        .and_then(Value::as_array)
        .expect("results array");
    assert_eq!(
        results.len(),
        1,
        "should stop after first non-zero exit_code"
    );
}

#[tokio::test]
async fn local_batch_stop_on_error_false_continues() {
    let _guard = ENV_LOCK.lock().await;

    let manager = LocalManager::new(Logger::new("test"), Validation::new(), Some(true));
    let result = manager
        .handle_action(serde_json::json!({
            "action": "batch",
            "stop_on_error": false,
            "commands": [
                { "command": "false" },
                { "command": "echo ok" }
            ]
        }))
        .await
        .expect("batch result");

    assert_eq!(result.get("success").and_then(Value::as_bool), Some(false));
    let results = result
        .get("results")
        .and_then(Value::as_array)
        .expect("results array");
    assert_eq!(results.len(), 2, "should continue when stop_on_error=false");
}

#[tokio::test]
async fn local_batch_parallel_runs_all() {
    let _guard = ENV_LOCK.lock().await;

    let manager = LocalManager::new(Logger::new("test"), Validation::new(), Some(true));
    let result = manager
        .handle_action(serde_json::json!({
            "action": "batch",
            "parallel": true,
            "commands": [
                { "command": "false" },
                { "command": "echo ok" }
            ]
        }))
        .await
        .expect("batch result");

    assert_eq!(result.get("success").and_then(Value::as_bool), Some(false));
    let results = result
        .get("results")
        .and_then(Value::as_array)
        .expect("results array");
    assert_eq!(results.len(), 2, "parallel mode must run all commands");
}

#[tokio::test]
async fn local_fs_write_stat_list_roundtrip() {
    let _guard = ENV_LOCK.lock().await;

    let root = tmp_dir("infra-local-fs");
    std::fs::create_dir_all(&root).expect("create dir");

    let file_path = root.join("hello.txt");
    let manager = LocalManager::new(Logger::new("test"), Validation::new(), Some(true));

    manager
        .handle_action(serde_json::json!({
            "action": "fs_write",
            "path": file_path.to_string_lossy(),
            "overwrite": true,
            "mode": 0o600,
            "content": "hello",
            "encoding": "utf8"
        }))
        .await
        .expect("fs_write");

    let stat = manager
        .handle_action(serde_json::json!({
            "action": "fs_stat",
            "path": file_path.to_string_lossy(),
        }))
        .await
        .expect("fs_stat");

    assert_eq!(stat.get("type").and_then(Value::as_str), Some("file"));
    assert_eq!(stat.get("size").and_then(Value::as_u64), Some(5));
    assert!(stat.get("mtime_ms").is_some(), "mtime_ms should be present");

    let list = manager
        .handle_action(serde_json::json!({
            "action": "fs_list",
            "path": root.to_string_lossy(),
            "recursive": true,
            "max_depth": 1,
            "with_stats": true
        }))
        .await
        .expect("fs_list");

    let entries = list
        .get("entries")
        .and_then(Value::as_array)
        .expect("entries");
    assert!(
        entries.iter().any(|item| item.get("path").is_some()),
        "entries should include full path"
    );
    assert!(
        entries.iter().any(|item| item.get("mtime_ms").is_some()),
        "with_stats should include mtime_ms"
    );
}

#[tokio::test]
async fn local_fs_rm_force_ignores_missing() {
    let _guard = ENV_LOCK.lock().await;

    let missing = tmp_dir("infra-local-missing").join("nope.txt");
    let manager = LocalManager::new(Logger::new("test"), Validation::new(), Some(true));

    let result = manager
        .handle_action(serde_json::json!({
            "action": "fs_rm",
            "path": missing.to_string_lossy(),
            "force": true
        }))
        .await
        .expect("fs_rm force");

    assert_eq!(result.get("success").and_then(Value::as_bool), Some(true));
}
