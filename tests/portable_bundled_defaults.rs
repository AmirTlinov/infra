use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

mod common;
use common::ENV_LOCK;

struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn copy_binary_to(dir: &Path) -> PathBuf {
    let src = PathBuf::from(env!("CARGO_BIN_EXE_infra"));
    let name = if cfg!(windows) { "infra.exe" } else { "infra" };
    let dst = dir.join(name);
    fs::copy(&src, &dst).expect("copy infra binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&dst).expect("binary metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dst, perms).expect("set executable bit");
    }
    dst
}

fn spawn_stdout_reader(
    stdout: impl std::io::Read + Send + 'static,
) -> Receiver<Result<String, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    let _ = tx.send(Err(err.to_string()));
                    break;
                }
            }
        }
    });
    rx
}

fn recv_json(rx: &Receiver<Result<String, String>>, timeout: Duration) -> Value {
    let line = match rx.recv_timeout(timeout) {
        Ok(Ok(line)) => line,
        Ok(Err(err)) => panic!("stdout read failed: {err}"),
        Err(err) => panic!("timed out waiting for stdout line: {err}"),
    };
    serde_json::from_str(line.trim()).expect("parse json-rpc response")
}

fn send(child: &mut Child, payload: Value) {
    let stdin = child.stdin.as_mut().expect("child stdin");
    writeln!(stdin, "{}", payload).expect("write request");
    stdin.flush().expect("flush request");
}

fn init_git_repo(repo_dir: &Path) {
    fs::create_dir_all(repo_dir).expect("create repo dir");
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo_dir)
            .env("GIT_AUTHOR_NAME", "Infra Test")
            .env("GIT_AUTHOR_EMAIL", "infra@example.com")
            .env("GIT_COMMITTER_NAME", "Infra Test")
            .env("GIT_COMMITTER_EMAIL", "infra@example.com")
            .status()
            .expect("run git");
        assert!(status.success(), "git {:?} should succeed", args);
    };

    git(&["init"]);
    fs::write(repo_dir.join("README.md"), "hello bundled defaults\n").expect("write repo file");
    git(&["add", "README.md"]);
    git(&["commit", "-m", "init"]);
}

#[test]
fn copied_binary_uses_bundled_manifests_and_repo_snapshot_stays_safe() {
    let _guard = ENV_LOCK.blocking_lock();

    let tmp_dir = std::env::temp_dir().join(format!(
        "infra-portable-bundled-test-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&tmp_dir).expect("create temp root");
    let portable_dir = tmp_dir.join("portable");
    let home_dir = tmp_dir.join("home");
    let state_dir = tmp_dir.join("xdg-state");
    let profiles_dir = tmp_dir.join("profiles");
    let repo_dir = tmp_dir.join("repo");
    fs::create_dir_all(&portable_dir).expect("create portable dir");
    fs::create_dir_all(&home_dir).expect("create home dir");
    fs::create_dir_all(&state_dir).expect("create xdg state dir");
    fs::create_dir_all(&profiles_dir).expect("create profiles dir");
    init_git_repo(&repo_dir);

    let portable_bin = copy_binary_to(&portable_dir);
    let mut child = ChildGuard {
        child: Command::new(&portable_bin)
            .current_dir(&portable_dir)
            .env("HOME", &home_dir)
            .env("XDG_STATE_HOME", &state_dir)
            .env("MCP_PROFILES_DIR", &profiles_dir)
            .env_remove("MCP_DEFAULT_RUNBOOKS_PATH")
            .env_remove("MCP_DEFAULT_CAPABILITIES_PATH")
            .env_remove("MCP_RUNBOOKS_PATH")
            .env_remove("MCP_CAPABILITIES_PATH")
            .env_remove("INFRA_UNSAFE_LOCAL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn copied infra binary"),
    };
    let stdout = child.child.stdout.take().expect("stdout");
    let rx = spawn_stdout_reader(stdout);

    send(
        &mut child.child,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "portable-bundled-test", "version": "0" }
            }
        }),
    );
    let init = recv_json(&rx, Duration::from_secs(5));
    assert_eq!(init.get("id").and_then(|v| v.as_i64()), Some(1));
    assert!(
        init.get("error").is_none(),
        "initialize should succeed: {init}"
    );

    send(
        &mut child.child,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    );

    send(
        &mut child.child,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "mcp_runbook",
                "arguments": { "action": "list", "query": "repo.snapshot", "limit": 5 }
            }
        }),
    );
    let runbook_list = recv_json(&rx, Duration::from_secs(5));
    let runbook_list_payload = runbook_list
        .pointer("/result/structuredContent/result")
        .and_then(|v| v.as_object())
        .cloned()
        .expect("runbook list structured result");
    let runbooks = runbook_list_payload
        .get("runbooks")
        .and_then(|v| v.as_array())
        .expect("runbooks array");
    assert!(
        runbooks.iter().any(|entry| {
            entry.get("name").and_then(|v| v.as_str()) == Some("repo.snapshot")
                && entry.get("manifest_source").and_then(|v| v.as_str()) == Some("bundled_manifest")
        }),
        "repo.snapshot bundled runbook should be present: {runbook_list}"
    );

    send(
        &mut child.child,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "mcp_capability",
                "arguments": { "action": "list", "query": "repo.snapshot", "limit": 5 }
            }
        }),
    );
    let capability = recv_json(&rx, Duration::from_secs(5));
    let capability_payload = capability
        .pointer("/result/structuredContent/result")
        .and_then(|v| v.as_object())
        .cloned()
        .expect("capability structured result");
    assert_eq!(
        capability_payload
            .get("manifest")
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("bundled_manifest")
    );
    let capabilities = capability_payload
        .get("capabilities")
        .and_then(|v| v.as_array())
        .expect("capabilities array");
    assert!(
        capabilities.iter().any(|entry| {
            entry.get("name").and_then(|v| v.as_str()) == Some("repo.snapshot")
                && entry.get("manifest_source").and_then(|v| v.as_str()) == Some("bundled_manifest")
        }),
        "repo.snapshot bundled capability should be present: {capability}"
    );

    send(
        &mut child.child,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "mcp_runbook",
                "arguments": {
                    "action": "run",
                    "name": "repo.snapshot",
                    "input": { "repo_path": repo_dir.to_string_lossy() }
                }
            }
        }),
    );
    let runbook = recv_json(&rx, Duration::from_secs(10));
    let runbook_payload = runbook
        .pointer("/result/structuredContent/result")
        .and_then(|v| v.as_object())
        .cloned()
        .expect("runbook structured result");
    assert_eq!(
        runbook_payload
            .get("effects")
            .and_then(|v| v.get("kind"))
            .and_then(|v| v.as_str()),
        Some("read")
    );
    assert_eq!(
        runbook_payload
            .get("effects")
            .and_then(|v| v.get("requires_apply"))
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        runbook_payload
            .get("runbook_manifest")
            .and_then(|v| v.get("manifest_source"))
            .and_then(|v| v.as_str()),
        Some("bundled_manifest")
    );
    assert_eq!(
        runbook_payload.get("success").and_then(|v| v.as_bool()),
        Some(true),
        "repo.snapshot should succeed without apply or unsafe local: {runbook}"
    );

    fs::remove_dir_all(&tmp_dir).ok();
}
