use serde_json::Value;
use std::path::Path;
use std::process::Command;

mod common;
use common::ENV_LOCK;

fn write_json(path: &Path, value: &Value) {
    let payload = serde_json::to_string_pretty(value).expect("serialize json");
    std::fs::write(path, format!("{payload}\n")).expect("write json");
}

fn run_cli(
    cwd: &Path,
    profiles_dir: &Path,
    extra_env: &[(&str, String)],
    args: &[&str],
) -> (i32, Value, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_infra"));
    command.current_dir(cwd);
    command.env("INFRA_PROFILES_DIR", profiles_dir);
    command.env_remove("INFRA_DEFAULT_RUNBOOKS_PATH");
    command.env_remove("INFRA_DEFAULT_CAPABILITIES_PATH");
    command.env_remove("INFRA_RUNBOOKS_PATH");
    command.env_remove("INFRA_CAPABILITIES_PATH");
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.args(args);
    let output = command.output().expect("run infra");
    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
    let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
    let value: Value = serde_json::from_str(stdout.trim()).expect("parse cli json");
    (output.status.code().unwrap_or(-1), value, stderr)
}

#[test]
fn cli_exposes_bundled_defaults_from_non_repo_cwd() {
    let _guard = ENV_LOCK.blocking_lock();
    let tmp_root = std::env::temp_dir().join(format!("infra-cli-bundled-{}", uuid::Uuid::new_v4()));
    let cwd = tmp_root.join("cwd");
    let profiles = tmp_root.join("profiles");
    std::fs::create_dir_all(&cwd).expect("create cwd");
    std::fs::create_dir_all(&profiles).expect("create profiles");

    let (status, describe, stderr) = run_cli(&cwd, &profiles, &[], &["describe", "status"]);
    assert_eq!(status, 0, "stderr: {stderr}");
    assert_eq!(
        describe.get("state").and_then(|v| v.as_str()),
        Some("ready")
    );
    assert_eq!(
        describe
            .pointer("/description_snapshot/sources/capabilities/manifest_source")
            .and_then(|v| v.as_str()),
        Some("bundled_manifest")
    );
    assert_eq!(
        describe
            .pointer("/description_snapshot/sources/runbooks/manifest_source")
            .and_then(|v| v.as_str()),
        Some("bundled_manifest")
    );

    let (status, capability, stderr) = run_cli(
        &cwd,
        &profiles,
        &[],
        &["capability", "get", "--arg", "name=repo.snapshot"],
    );
    assert_eq!(status, 0, "stderr: {stderr}");
    assert_eq!(
        capability
            .pointer("/result/capability/manifest_source")
            .and_then(|v| v.as_str()),
        Some("bundled_manifest")
    );

    let (status, runbook, stderr) = run_cli(
        &cwd,
        &profiles,
        &[],
        &["runbook", "get", "--arg", "name=repo.snapshot"],
    );
    assert_eq!(status, 0, "stderr: {stderr}");
    assert_eq!(
        runbook
            .pointer("/result/runbook/manifest_source")
            .and_then(|v| v.as_str()),
        Some("bundled_manifest")
    );
}

#[test]
fn cli_sees_manifest_updates_without_manual_refresh() {
    let _guard = ENV_LOCK.blocking_lock();
    let tmp_root = std::env::temp_dir().join(format!("infra-cli-refresh-{}", uuid::Uuid::new_v4()));
    let cwd = tmp_root.join("cwd");
    let profiles = tmp_root.join("profiles");
    let runbooks_path = tmp_root.join("runbooks.json");
    let capabilities_path = tmp_root.join("capabilities.json");
    std::fs::create_dir_all(&cwd).expect("create cwd");
    std::fs::create_dir_all(&profiles).expect("create profiles");

    write_json(
        &runbooks_path,
        &serde_json::json!({
            "demo.observe": {
                "steps": [{ "tool": "state", "args": { "action": "get", "key": "demo", "scope": "session" } }]
            }
        }),
    );
    write_json(
        &capabilities_path,
        &serde_json::json!({
            "version": 1,
            "capabilities": {
                "demo.observe": {
                    "intent": "demo.observe",
                    "description": "version one",
                    "runbook": "demo.observe",
                    "when": {},
                    "inputs": { "required": [], "defaults": {}, "map": {} },
                    "effects": { "kind": "read", "requires_apply": false }
                }
            }
        }),
    );

    let env = vec![
        (
            "INFRA_DEFAULT_RUNBOOKS_PATH",
            runbooks_path.to_string_lossy().to_string(),
        ),
        (
            "INFRA_DEFAULT_CAPABILITIES_PATH",
            capabilities_path.to_string_lossy().to_string(),
        ),
    ];

    let (status, first, stderr) = run_cli(
        &cwd,
        &profiles,
        &env,
        &["capability", "get", "--arg", "name=demo.observe"],
    );
    assert_eq!(status, 0, "stderr: {stderr}");
    let first_hash = first
        .pointer("/description_snapshot/hash")
        .and_then(|v| v.as_str())
        .expect("first hash")
        .to_string();
    assert_eq!(
        first
            .pointer("/result/capability/description")
            .and_then(|v| v.as_str()),
        Some("version one")
    );

    write_json(
        &capabilities_path,
        &serde_json::json!({
            "version": 2,
            "capabilities": {
                "demo.observe": {
                    "intent": "demo.observe",
                    "description": "version two",
                    "runbook": "demo.observe",
                    "when": {},
                    "inputs": { "required": [], "defaults": {}, "map": {} },
                    "effects": { "kind": "read", "requires_apply": false }
                }
            }
        }),
    );

    let (status, second, stderr) = run_cli(
        &cwd,
        &profiles,
        &env,
        &["capability", "get", "--arg", "name=demo.observe"],
    );
    assert_eq!(status, 0, "stderr: {stderr}");
    assert_eq!(
        second
            .pointer("/result/capability/description")
            .and_then(|v| v.as_str()),
        Some("version two")
    );
    assert_eq!(
        second
            .pointer("/description_snapshot/version/capabilities")
            .cloned(),
        Some(serde_json::json!(2))
    );
    let second_hash = second
        .pointer("/description_snapshot/hash")
        .and_then(|v| v.as_str())
        .expect("second hash");
    assert_ne!(first_hash, second_hash);
}

#[test]
fn cli_verify_requires_explicit_checks() {
    let _guard = ENV_LOCK.blocking_lock();
    let tmp_root = std::env::temp_dir().join(format!("infra-cli-verify-{}", uuid::Uuid::new_v4()));
    let cwd = tmp_root.join("cwd");
    let profiles = tmp_root.join("profiles");
    let runbooks_path = tmp_root.join("runbooks.json");
    let capabilities_path = tmp_root.join("capabilities.json");
    std::fs::create_dir_all(&cwd).expect("create cwd");
    std::fs::create_dir_all(&profiles).expect("create profiles");

    write_json(
        &runbooks_path,
        &serde_json::json!({
            "demo.observe": {
                "steps": [{ "tool": "state", "args": { "action": "get", "key": "demo", "scope": "session" } }]
            }
        }),
    );
    write_json(
        &capabilities_path,
        &serde_json::json!({
            "version": 1,
            "capabilities": {
                "demo.observe": {
                    "intent": "demo.observe",
                    "description": "demo observe",
                    "runbook": "demo.observe",
                    "when": {},
                    "inputs": { "required": [], "defaults": {}, "map": {} },
                    "effects": { "kind": "read", "requires_apply": false }
                }
            }
        }),
    );

    let env = vec![
        (
            "INFRA_DEFAULT_RUNBOOKS_PATH",
            runbooks_path.to_string_lossy().to_string(),
        ),
        (
            "INFRA_DEFAULT_CAPABILITIES_PATH",
            capabilities_path.to_string_lossy().to_string(),
        ),
    ];

    let (status, observed, stderr) = run_cli(
        &cwd,
        &profiles,
        &env,
        &["operation", "observe", "--arg", "family=demo"],
    );
    assert_eq!(status, 0, "stderr: {stderr}");
    assert_eq!(
        observed.get("state").and_then(|v| v.as_str()),
        Some("completed")
    );

    let (status, verify_missing, _stderr) = run_cli(
        &cwd,
        &profiles,
        &env,
        &["operation", "verify", "--arg", "family=demo"],
    );
    assert_eq!(status, 30);
    assert_eq!(
        verify_missing.get("state").and_then(|v| v.as_str()),
        Some("blocked")
    );
    assert_eq!(
        verify_missing
            .pointer("/error/message")
            .and_then(|v| v.as_str()),
        Some("verify requires explicit checks")
    );

    let (status, verified, stderr) = run_cli(
        &cwd,
        &profiles,
        &env,
        &[
            "operation",
            "verify",
            "--arg",
            "family=demo",
            "--arg",
            "checks=[{\"path\":\"results.0.result.success\",\"equals\":true}]",
        ],
    );
    assert_eq!(status, 0, "stderr: {stderr}");
    assert_eq!(
        verified.get("state").and_then(|v| v.as_str()),
        Some("completed")
    );
    assert_eq!(
        verified
            .pointer("/receipt/verification/passed")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn cli_ambiguous_capability_resolution_fails_loudly() {
    let _guard = ENV_LOCK.blocking_lock();
    let tmp_root =
        std::env::temp_dir().join(format!("infra-cli-ambiguous-{}", uuid::Uuid::new_v4()));
    let cwd = tmp_root.join("cwd");
    let profiles = tmp_root.join("profiles");
    let capabilities_path = tmp_root.join("capabilities.json");
    std::fs::create_dir_all(&cwd).expect("create cwd");
    std::fs::create_dir_all(&profiles).expect("create profiles");

    write_json(
        &capabilities_path,
        &serde_json::json!({
            "version": 1,
            "capabilities": {
                "demo.observe": {
                    "intent": "demo.observe",
                    "description": "observe A",
                    "runbook": "repo.snapshot",
                    "when": {},
                    "inputs": { "required": [], "defaults": {}, "map": {} },
                    "effects": { "kind": "read", "requires_apply": false }
                },
                "demo.inspect": {
                    "intent": "demo.observe",
                    "description": "observe B",
                    "runbook": "repo.snapshot",
                    "when": {},
                    "inputs": { "required": [], "defaults": {}, "map": {} },
                    "effects": { "kind": "read", "requires_apply": false }
                }
            }
        }),
    );

    let env = vec![(
        "INFRA_DEFAULT_CAPABILITIES_PATH",
        capabilities_path.to_string_lossy().to_string(),
    )];
    let (status, output, _stderr) = run_cli(
        &cwd,
        &profiles,
        &env,
        &["capability", "resolve", "--arg", "intent=demo.observe"],
    );
    assert_eq!(status, 30);
    assert_eq!(
        output.get("state").and_then(|v| v.as_str()),
        Some("ambiguous")
    );
    assert_eq!(
        output.pointer("/error/code").and_then(|v| v.as_str()),
        Some("AMBIGUOUS_CAPABILITY")
    );
    assert_eq!(
        output
            .pointer("/error/details/candidates")
            .and_then(|v| v.as_array())
            .map(|items| items.len()),
        Some(2)
    );
}
