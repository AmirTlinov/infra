use serde_json::Value;
use std::io::{BufRead, Read, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

mod common;
use common::ENV_LOCK;

struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn spawn(idle_timeout_ms: &str, profiles_dir: &std::path::Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_infra"))
            .env("INFRA_APP_IDLE_UNLOAD_MS", idle_timeout_ms)
            .env_remove("INFRA_STDIO_IDLE_TIMEOUT_MS")
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

fn help_request(id: i64) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "help",
            "arguments": {}
        }
    })
    .to_string()
        + "\n"
}

fn state_set_request(id: i64, key: &str, value: Value, scope: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "mcp_state",
            "arguments": {
                "action": "set",
                "key": key,
                "value": value,
                "scope": scope
            }
        }
    })
    .to_string()
        + "\n"
}

fn state_get_request(id: i64, key: &str, scope: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "mcp_state",
            "arguments": {
                "action": "get",
                "key": key,
                "scope": scope
            }
        }
    })
    .to_string()
        + "\n"
}

fn target_list_for_project_request(id: i64, project_name: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "mcp_target",
            "arguments": {
                "action": "list",
                "project_name": project_name
            }
        }
    })
    .to_string()
        + "\n"
}

fn project_upsert_request(id: i64, name: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "mcp_project",
            "arguments": {
                "action": "project_upsert",
                "name": name,
                "description": "Demo project",
                "default_target": "staging",
                "targets": {
                    "staging": {
                        "ssh_profile": "demo-ssh",
                        "cwd": "/srv/demo"
                    }
                }
            }
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
        "params": {}
    })
    .to_string()
        + "\n"
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

fn wait_for_exit(child: &mut Child, timeout: Duration, context: &str) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll child exit") {
            return status;
        }
        if Instant::now() >= deadline {
            panic!("{context}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(target_os = "linux")]
fn list_thread_names(pid: u32) -> Vec<String> {
    let mut names = Vec::new();
    let task_dir = std::path::PathBuf::from(format!("/proc/{pid}/task"));
    for entry in std::fs::read_dir(task_dir).expect("read /proc task dir") {
        let entry = entry.expect("task entry");
        let comm_path = entry.path().join("comm");
        if let Ok(name) = std::fs::read_to_string(comm_path) {
            names.push(name.trim().to_string());
        }
    }
    names.sort();
    names
}

#[cfg(target_os = "linux")]
#[test]
fn infra_binary_uses_bounded_idle_threads_before_first_request() {
    let _guard = ENV_LOCK.blocking_lock();

    let tmp_dir =
        std::env::temp_dir().join(format!("infra-stdio-idle-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let mut child = ChildGuard::spawn("100", &tmp_dir);
    let pid = child.child.id();

    std::thread::sleep(Duration::from_millis(150));
    let names = list_thread_names(pid);
    assert!(
        names.len() <= 3,
        "expected a bounded thread count before first request, got {} threads: {:?}",
        names.len(),
        names
    );
    assert!(
        !names.iter().any(|name| name.starts_with("tokio-runtime-w")),
        "expected no multi-thread tokio workers before first request, got: {:?}",
        names
    );

    drop(child.child.stdin.take());
    let status = wait_for_exit(
        &mut child.child,
        Duration::from_secs(2),
        "infra binary should exit after stdin EOF",
    );
    assert!(
        status.success(),
        "infra binary should exit successfully after EOF"
    );

    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn infra_binary_unloads_runtime_after_idle_but_keeps_transport_alive() {
    let _guard = ENV_LOCK.blocking_lock();

    let tmp_dir =
        std::env::temp_dir().join(format!("infra-stdio-idle-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let mut child = ChildGuard::spawn("100", &tmp_dir);
    let mut stdin = child.child.stdin.take().expect("child stdin");
    let stdout = child.child.stdout.take().expect("child stdout");
    let mut stderr = child.child.stderr.take().expect("child stderr");
    let stdout_rx = spawn_stdout_reader(stdout);

    stdin
        .write_all(help_request(1).as_bytes())
        .expect("write initial help request");
    stdin.flush().expect("flush initial help request");

    let response_line =
        recv_stdout_line(&stdout_rx, Duration::from_secs(2)).expect("help response");
    let response: Value = serde_json::from_str(response_line.trim()).expect("parse response json");
    assert_eq!(response.get("id"), Some(&serde_json::json!(1)));
    assert!(response.get("result").is_some());

    std::thread::sleep(Duration::from_millis(250));
    assert!(
        child.child.try_wait().expect("poll child exit").is_none(),
        "infra binary should remain alive after idle unload so the same transport can be reused"
    );

    stdin
        .write_all(help_request(2).as_bytes())
        .expect("write second help request");
    stdin.flush().expect("flush second help request");

    let second_line = recv_stdout_line(&stdout_rx, Duration::from_secs(2))
        .expect("help response after idle unload");
    let second_response: Value =
        serde_json::from_str(second_line.trim()).expect("parse second response json");
    assert_eq!(second_response.get("id"), Some(&serde_json::json!(2)));
    assert!(second_response.get("result").is_some());

    drop(stdin);
    let status = wait_for_exit(
        &mut child.child,
        Duration::from_secs(2),
        "infra binary should exit after stdin EOF",
    );
    assert!(
        status.success(),
        "infra binary should exit successfully after EOF"
    );

    let mut stderr_text = String::new();
    stderr
        .read_to_string(&mut stderr_text)
        .expect("read stderr after exit");
    assert!(
        stderr_text.contains("infra: app idle timeout reached (100 ms), unloading runtime state"),
        "expected runtime-unload stderr message, got: {stderr_text}"
    );
    assert!(
        !stderr_text.contains("stdio idle timeout reached"),
        "old self-terminate message should be absent, got: {stderr_text}"
    );

    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn infra_binary_preserves_session_state_across_idle_unload() {
    let _guard = ENV_LOCK.blocking_lock();

    let tmp_dir =
        std::env::temp_dir().join(format!("infra-stdio-session-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let mut child = ChildGuard::spawn("100", &tmp_dir);
    let mut stdin = child.child.stdin.take().expect("child stdin");
    let stdout = child.child.stdout.take().expect("child stdout");
    let mut stderr = child.child.stderr.take().expect("child stderr");
    let stdout_rx = spawn_stdout_reader(stdout);

    stdin
        .write_all(initialize_request().as_bytes())
        .expect("write initialize request");
    stdin
        .write_all(
            state_set_request(2, "session.probe", serde_json::json!(123), "session").as_bytes(),
        )
        .expect("write state set request");
    stdin.flush().expect("flush initialize + state set");

    let initialize_line =
        recv_stdout_line(&stdout_rx, Duration::from_secs(2)).expect("initialize response");
    let initialize_response: Value =
        serde_json::from_str(initialize_line.trim()).expect("parse initialize response json");
    assert_eq!(initialize_response.get("id"), Some(&serde_json::json!(1)));

    let set_line =
        recv_stdout_line(&stdout_rx, Duration::from_secs(2)).expect("state set response");
    let set_response: Value =
        serde_json::from_str(set_line.trim()).expect("parse state set response json");
    assert_eq!(set_response.get("id"), Some(&serde_json::json!(2)));

    std::thread::sleep(Duration::from_millis(250));
    assert!(
        child.child.try_wait().expect("poll child exit").is_none(),
        "infra binary should stay alive while unloaded runtime waits for more input"
    );

    stdin
        .write_all(state_get_request(3, "session.probe", "session").as_bytes())
        .expect("write state get request");
    stdin.flush().expect("flush state get request");

    let get_line =
        recv_stdout_line(&stdout_rx, Duration::from_secs(2)).expect("state get after idle unload");
    let get_response: Value =
        serde_json::from_str(get_line.trim()).expect("parse state get response json");
    assert_eq!(get_response.get("id"), Some(&serde_json::json!(3)));
    assert_eq!(
        get_response
            .get("result")
            .and_then(|v| v.get("structuredContent"))
            .and_then(|v| v.get("result"))
            .and_then(|v| v.get("value")),
        Some(&serde_json::json!(123)),
        "session state should survive app unload and reload within the same stdio session"
    );

    drop(stdin);
    let status = wait_for_exit(
        &mut child.child,
        Duration::from_secs(2),
        "infra binary should exit after stdin EOF",
    );
    assert!(
        status.success(),
        "infra binary should exit successfully after EOF"
    );

    let mut stderr_text = String::new();
    stderr
        .read_to_string(&mut stderr_text)
        .expect("read stderr after exit");
    assert!(
        stderr_text.contains("infra: app idle timeout reached (100 ms), unloading runtime state"),
        "expected runtime-unload stderr message, got: {stderr_text}"
    );

    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn infra_binary_rehydrates_general_tool_calls_after_idle_unload() {
    let _guard = ENV_LOCK.blocking_lock();

    let tmp_dir =
        std::env::temp_dir().join(format!("infra-stdio-idle-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let mut child = ChildGuard::spawn("100", &tmp_dir);
    let mut stdin = child.child.stdin.take().expect("child stdin");
    let stdout = child.child.stdout.take().expect("child stdout");
    let stdout_rx = spawn_stdout_reader(stdout);

    stdin
        .write_all(project_upsert_request(1, "demo").as_bytes())
        .expect("write project upsert request");
    stdin.flush().expect("flush project upsert request");

    let project_line =
        recv_stdout_line(&stdout_rx, Duration::from_secs(2)).expect("project upsert response");
    let project_response: Value =
        serde_json::from_str(project_line.trim()).expect("parse project upsert json");
    assert_eq!(project_response.get("id"), Some(&serde_json::json!(1)));
    assert!(project_response.get("result").is_some());

    std::thread::sleep(Duration::from_millis(250));
    assert!(
        child.child.try_wait().expect("poll child exit").is_none(),
        "infra binary should remain alive after idle unload so follow-up tool calls can reuse the same transport"
    );

    stdin
        .write_all(target_list_for_project_request(2, "demo").as_bytes())
        .expect("write target list request");
    stdin.flush().expect("flush target list request");

    let target_line =
        recv_stdout_line(&stdout_rx, Duration::from_secs(2)).expect("target list response");
    let target_response: Value =
        serde_json::from_str(target_line.trim()).expect("parse target list json");
    assert_eq!(target_response.get("id"), Some(&serde_json::json!(2)));
    assert!(target_response.get("result").is_some());
    assert_eq!(
        target_response
            .get("result")
            .and_then(|v| v.get("structuredContent"))
            .and_then(|v| v.get("result"))
            .and_then(|v| v.get("targets"))
            .and_then(|v| v.as_array())
            .map(|targets| targets.len()),
        Some(1),
        "rehydrated target list should resolve against the seeded project after idle unload"
    );

    drop(stdin);
    let status = wait_for_exit(
        &mut child.child,
        Duration::from_secs(2),
        "infra binary should exit after stdin EOF",
    );
    assert!(
        status.success(),
        "infra binary should exit successfully after EOF"
    );

    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn infra_binary_waits_for_eof_when_idle_timeout_is_disabled() {
    let _guard = ENV_LOCK.blocking_lock();

    let tmp_dir =
        std::env::temp_dir().join(format!("infra-stdio-idle-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let mut child = ChildGuard::spawn("0", &tmp_dir);
    let mut stdin = child.child.stdin.take().expect("child stdin");
    let stdout = child.child.stdout.take().expect("child stdout");
    let mut stderr = child.child.stderr.take().expect("child stderr");
    let stdout_rx = spawn_stdout_reader(stdout);

    stdin
        .write_all(initialize_request().as_bytes())
        .expect("write initialize request");
    stdin.flush().expect("flush initialize request");

    let response_line =
        recv_stdout_line(&stdout_rx, Duration::from_secs(2)).expect("initialize response");
    let response: Value = serde_json::from_str(response_line.trim()).expect("parse response json");
    assert_eq!(response.get("id"), Some(&serde_json::json!(1)));
    assert!(response.get("result").is_some());

    std::thread::sleep(Duration::from_millis(250));
    assert!(
        child.child.try_wait().expect("poll child exit").is_none(),
        "infra binary should stay alive while stdin pipe stays open when idle timeout is disabled"
    );

    drop(stdin);
    let status = wait_for_exit(
        &mut child.child,
        Duration::from_secs(2),
        "infra binary should exit after stdin EOF when idle timeout is disabled",
    );
    assert!(
        status.success(),
        "infra binary should exit successfully after EOF"
    );

    let mut stderr_text = String::new();
    stderr
        .read_to_string(&mut stderr_text)
        .expect("read stderr after exit");
    assert!(
        !stderr_text.contains("app idle timeout reached"),
        "idle-unload log should be absent when timeout is disabled, got: {stderr_text}"
    );

    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn stdio_sessions_keep_session_state_isolated_per_process() {
    let _guard = ENV_LOCK.blocking_lock();

    let tmp_dir = std::env::temp_dir().join(format!(
        "infra-stdio-isolation-test-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let mut child_a = ChildGuard::spawn("100", &tmp_dir);
    let mut child_b = ChildGuard::spawn("100", &tmp_dir);

    let mut stdin_a = child_a.child.stdin.take().expect("child A stdin");
    let stdout_a = child_a.child.stdout.take().expect("child A stdout");
    let stdout_lines_a = spawn_stdout_reader(stdout_a);

    let mut stdin_b = child_b.child.stdin.take().expect("child B stdin");
    let stdout_b = child_b.child.stdout.take().expect("child B stdout");
    let stdout_lines_b = spawn_stdout_reader(stdout_b);

    stdin_a
        .write_all(initialize_request().as_bytes())
        .expect("write child A initialize");
    stdin_a
        .write_all(
            state_set_request(2, "session.probe", serde_json::json!(123), "session").as_bytes(),
        )
        .expect("write child A set-state");
    stdin_a.flush().expect("flush child A");

    let init_a =
        recv_stdout_line(&stdout_lines_a, Duration::from_secs(2)).expect("child A initialize");
    let init_a_value: Value =
        serde_json::from_str(init_a.trim()).expect("parse child A initialize response");
    assert_eq!(init_a_value.get("id"), Some(&serde_json::json!(1)));

    let set_a =
        recv_stdout_line(&stdout_lines_a, Duration::from_secs(2)).expect("child A set-state");
    let set_a_value: Value =
        serde_json::from_str(set_a.trim()).expect("parse child A set-state response");
    assert_eq!(set_a_value.get("id"), Some(&serde_json::json!(2)));
    assert!(
        set_a_value
            .get("result")
            .and_then(|v| v.get("structuredContent"))
            .and_then(|v| v.get("success"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "child A session state set should succeed"
    );

    stdin_b
        .write_all(initialize_request().as_bytes())
        .expect("write child B initialize");
    stdin_b
        .write_all(state_get_request(2, "session.probe", "session").as_bytes())
        .expect("write child B get-state");
    stdin_b.flush().expect("flush child B");

    let init_b =
        recv_stdout_line(&stdout_lines_b, Duration::from_secs(2)).expect("child B initialize");
    let init_b_value: Value =
        serde_json::from_str(init_b.trim()).expect("parse child B initialize response");
    assert_eq!(init_b_value.get("id"), Some(&serde_json::json!(1)));

    let get_b =
        recv_stdout_line(&stdout_lines_b, Duration::from_secs(2)).expect("child B get-state");
    let get_b_value: Value =
        serde_json::from_str(get_b.trim()).expect("parse child B get-state response");
    assert_eq!(get_b_value.get("id"), Some(&serde_json::json!(2)));
    assert_eq!(
        get_b_value
            .get("result")
            .and_then(|v| v.get("structuredContent"))
            .and_then(|v| v.get("result"))
            .and_then(|v| v.get("value")),
        Some(&Value::Null),
        "session-scoped state must remain process-local even under shared MCP_PROFILES_DIR"
    );

    drop(stdin_a);
    drop(stdin_b);
    let status_a = wait_for_exit(
        &mut child_a.child,
        Duration::from_secs(2),
        "child A should exit after stdin EOF",
    );
    let status_b = wait_for_exit(
        &mut child_b.child,
        Duration::from_secs(2),
        "child B should exit after stdin EOF",
    );
    assert!(
        status_a.success(),
        "child A should exit successfully after EOF"
    );
    assert!(
        status_b.success(),
        "child B should exit successfully after EOF"
    );

    std::fs::remove_dir_all(&tmp_dir).ok();
}
