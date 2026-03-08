use serde_json::Value;
use std::io::{BufRead, Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod common;
use common::ENV_LOCK;

struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn spawn(idle_timeout_ms: &str, profiles_dir: &std::path::Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_infra"))
            .env("INFRA_STDIO_IDLE_TIMEOUT_MS", idle_timeout_ms)
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

fn read_first_stdout_line(
    stdout: std::process::ChildStdout,
    timeout: Duration,
) -> Result<String, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = std::io::BufReader::new(stdout)
            .read_line(&mut line)
            .map_err(|err| err.to_string())
            .map(|_| line);
        let _ = tx.send(result);
    });

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

#[test]
fn infra_binary_self_terminates_after_idle_timeout_with_pipe_left_open() {
    let _guard = ENV_LOCK.blocking_lock();

    let tmp_dir =
        std::env::temp_dir().join(format!("infra-stdio-idle-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let mut child = ChildGuard::spawn("100", &tmp_dir);
    let mut stdin = child.child.stdin.take().expect("child stdin");
    let stdout = child.child.stdout.take().expect("child stdout");
    let mut stderr = child.child.stderr.take().expect("child stderr");

    stdin
        .write_all(initialize_request().as_bytes())
        .expect("write initialize request");
    stdin.flush().expect("flush initialize request");

    let response_line =
        read_first_stdout_line(stdout, Duration::from_secs(2)).expect("initialize response");
    let response: Value = serde_json::from_str(response_line.trim()).expect("parse response json");
    assert_eq!(response.get("id"), Some(&serde_json::json!(1)));
    assert!(response.get("result").is_some());

    let status = wait_for_exit(
        &mut child.child,
        Duration::from_secs(2),
        "infra binary should exit after bounded stdio idle timeout",
    );
    assert!(
        status.success(),
        "idle-timed-out infra binary should exit successfully"
    );

    let mut stderr_text = String::new();
    stderr
        .read_to_string(&mut stderr_text)
        .expect("read stderr after exit");
    assert!(
        stderr_text.contains("infra: stdio idle timeout reached (100 ms), exiting"),
        "expected idle-timeout stderr message, got: {stderr_text}"
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

    stdin
        .write_all(initialize_request().as_bytes())
        .expect("write initialize request");
    stdin.flush().expect("flush initialize request");

    let response_line =
        read_first_stdout_line(stdout, Duration::from_secs(2)).expect("initialize response");
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
        !stderr_text.contains("stdio idle timeout reached"),
        "idle-timeout log should be absent when timeout is disabled, got: {stderr_text}"
    );

    std::fs::remove_dir_all(&tmp_dir).ok();
}
