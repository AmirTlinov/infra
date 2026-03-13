use serde_json::Value;
use std::io::{BufRead, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::Duration;

mod common;
use common::ENV_LOCK;

struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn spawn(profiles_dir: &std::path::Path, cwd: &std::path::Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_infra"))
            .current_dir(cwd)
            .env("INFRA_TOOL_TIER", "expert")
            .env("MCP_PROFILES_DIR", profiles_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn infra binary");
        Self { child }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn spawn_stdout_reader(stdout: ChildStdout) -> std::sync::mpsc::Receiver<Result<String, String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
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

fn recv_stdout_line(
    rx: &std::sync::mpsc::Receiver<Result<String, String>>,
    timeout: Duration,
) -> Result<String, String> {
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(err) => Err(format!("timed out waiting for stdout line: {err}")),
    }
}

fn call_request(id: i64, name: &str, arguments: Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments,
        }
    })
    .to_string()
        + "\n"
}

fn initialize_request() -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "installed-defaults-test", "version": "0"}
        }
    })
    .to_string()
        + "\n"
}

fn parse_call_text(response_line: &str) -> Value {
    let response: Value = serde_json::from_str(response_line.trim()).expect("parse response");
    let text = response
        .get("result")
        .and_then(|v| v.get("content"))
        .and_then(|v| v.as_array())
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(|v| v.as_str())
        .expect("text content");
    serde_json::from_str(text).expect("parse call payload")
}

#[test]
fn installed_binary_exposes_bundled_defaults_from_non_repo_cwd() {
    let _guard = ENV_LOCK.blocking_lock();

    let tmp_root =
        std::env::temp_dir().join(format!("infra-installed-defaults-{}", uuid::Uuid::new_v4()));
    let cwd = tmp_root.join("cwd");
    let profiles = tmp_root.join("profiles");
    std::fs::create_dir_all(&cwd).expect("create cwd");
    std::fs::create_dir_all(&profiles).expect("create profiles");

    let mut child = ChildGuard::spawn(&profiles, &cwd);
    let mut stdin = child.child.stdin.take().expect("stdin");
    let stdout = child.child.stdout.take().expect("stdout");
    let stdout_rx = spawn_stdout_reader(stdout);

    stdin
        .write_all(initialize_request().as_bytes())
        .expect("write initialize");
    stdin.flush().expect("flush initialize");

    let init_line =
        recv_stdout_line(&stdout_rx, Duration::from_secs(5)).expect("initialize response");
    let init_response: Value = serde_json::from_str(init_line.trim()).expect("parse initialize");
    assert_eq!(init_response.get("id"), Some(&serde_json::json!(1)));

    stdin
        .write_all(
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            })
            .to_string()
            .as_bytes(),
        )
        .expect("write initialized notification");
    stdin.write_all(b"\n").expect("newline");
    stdin.flush().expect("flush initialized");

    stdin
        .write_all(
            call_request(
                2,
                "mcp_runbook",
                serde_json::json!({"action": "runbook_get", "name": "repo.snapshot"}),
            )
            .as_bytes(),
        )
        .expect("write runbook get");
    stdin.flush().expect("flush runbook get");
    let runbook = parse_call_text(
        &recv_stdout_line(&stdout_rx, Duration::from_secs(5)).expect("runbook get response"),
    );
    let runbook_entry = runbook
        .get("result")
        .and_then(|v| v.get("runbook"))
        .expect("runbook entry");
    assert_eq!(
        runbook_entry.get("name").and_then(|v| v.as_str()),
        Some("repo.snapshot")
    );
    assert_eq!(
        runbook_entry
            .get("manifest_source")
            .and_then(|v| v.as_str()),
        Some("bundled_manifest")
    );
    assert_eq!(
        runbook_entry.get("manifest_path").and_then(|v| v.as_str()),
        Some("bundled://runbooks.json")
    );

    stdin
        .write_all(
            call_request(
                3,
                "mcp_capability",
                serde_json::json!({"action": "get", "name": "repo.snapshot"}),
            )
            .as_bytes(),
        )
        .expect("write capability get");
    stdin.flush().expect("flush capability get");
    let capability = parse_call_text(
        &recv_stdout_line(&stdout_rx, Duration::from_secs(5)).expect("capability get response"),
    );
    let capability_entry = capability
        .get("result")
        .and_then(|v| v.get("capability"))
        .expect("capability entry");
    assert_eq!(
        capability_entry.get("name").and_then(|v| v.as_str()),
        Some("repo.snapshot")
    );
    assert_eq!(
        capability_entry
            .get("manifest_source")
            .and_then(|v| v.as_str()),
        Some("bundled_manifest")
    );
    assert_eq!(
        capability_entry
            .get("manifest_path")
            .and_then(|v| v.as_str()),
        Some("bundled://capabilities.json")
    );

    drop(stdin);
    let status = child.child.wait().expect("wait child");
    assert!(
        status.success(),
        "infra binary should exit cleanly after stdin EOF"
    );

    std::fs::remove_dir_all(&tmp_root).ok();
}
