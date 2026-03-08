use serde_json::Value;
use std::process::Command;

mod common;
use common::ENV_LOCK;

#[tokio::test]
async fn baseline_separates_legacy_import_json_from_file_backed_manifests() {
    let _guard = ENV_LOCK.lock().await;

    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let tmp_dir =
        std::env::temp_dir().join(format!("infra-baseline-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    let out_path = tmp_dir.join("baseline.json");

    let status = Command::new(repo_root.join("tools/baseline"))
        .arg("--out")
        .arg(&out_path)
        .current_dir(repo_root)
        .status()
        .expect("run baseline tool");
    assert!(status.success(), "baseline tool should succeed");

    let payload: Value =
        serde_json::from_str(&std::fs::read_to_string(&out_path).expect("read baseline output"))
            .expect("parse baseline output");

    let legacy = payload
        .get("legacy_import_only_json_sources")
        .and_then(|v| v.as_array())
        .expect("legacy sources");
    let manifests = payload
        .get("file_backed_manifest_sources")
        .and_then(|v| v.as_array())
        .expect("manifest sources");

    let legacy_names: Vec<&str> = legacy.iter().filter_map(|v| v.as_str()).collect();
    let manifest_names: Vec<&str> = manifests.iter().filter_map(|v| v.as_str()).collect();

    assert!(!legacy_names.contains(&"runbooks.json"));
    assert!(!legacy_names.contains(&"capabilities.json"));
    assert!(manifest_names.contains(&"runbooks.json"));
    assert!(manifest_names.contains(&"capabilities.json"));

    std::fs::remove_dir_all(&tmp_dir).ok();
}
