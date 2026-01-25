use crate::errors::ToolError;
use crate::utils::user_paths::expand_home_path;
use futures::future::join_all;
use serde_json::Value;
use std::path::PathBuf;
use tokio::io::{copy, AsyncReadExt, AsyncWriteExt};

use super::{random_token, read_positive_int, LocalManager};
use crate::utils::stdin::{resolve_stdin_source, StdinSource};

const DEFAULT_STDOUT_INLINE_BYTES: usize = 32 * 1024;
const DEFAULT_STDERR_INLINE_BYTES: usize = 16 * 1024;

fn build_temp_dir() -> PathBuf {
    std::env::temp_dir().join(format!("infra-local-{}", std::process::id()))
}

impl LocalManager {
    pub(super) async fn exec(&self, args: Value) -> Result<Value, ToolError> {
        let command = self.validation.ensure_string(
            args.get("command").unwrap_or(&Value::Null),
            "command",
            false,
        )?;
        let argv = args.get("args").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .map(|v| v.as_str().unwrap_or(&v.to_string()).to_string())
                .collect::<Vec<_>>()
        });
        let cwd = args
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(expand_home_path);
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(|v| v.as_i64())
            .map(|v| v as u64);
        let stdin = resolve_stdin_source(&args)?;
        let stdin_eof = args
            .get("stdin_eof")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let inline = args
            .get("inline")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let shell_value = args.get("shell");
        let (use_shell, shell_program) = match shell_value {
            Some(Value::Bool(flag)) => (Some(*flag), None),
            Some(Value::String(text)) => {
                let normalized = text.trim().to_lowercase();
                let program = match normalized.as_str() {
                    "" => return Err(ToolError::invalid_params("shell must not be empty")),
                    "sh" => "sh",
                    "bash" => "bash",
                    other => {
                        return Err(ToolError::invalid_params(format!(
                            "shell: unsupported value '{}'",
                            other
                        )))
                    }
                };
                (Some(true), Some(program.to_string()))
            }
            Some(Value::Null) | None => (None, None),
            Some(_) => {
                return Err(ToolError::invalid_params(
                    "shell must be a boolean or string",
                ))
            }
        };
        let use_shell = use_shell.unwrap_or(argv.is_none());

        let mut cmd = if use_shell {
            let mut cmd = tokio::process::Command::new(shell_program.as_deref().unwrap_or("sh"));
            cmd.arg("-c").arg(command.clone());
            cmd
        } else {
            let mut cmd = tokio::process::Command::new(&command);
            if let Some(args_vec) = argv.as_ref() {
                cmd.args(args_vec);
            }
            cmd
        };

        if let Some(cwd) = cwd.as_ref() {
            cmd.current_dir(cwd);
        }

        if let Some(env) = self.normalize_env(args.get("env"))? {
            for (key, value) in env {
                cmd.env(key, value);
            }
        }

        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|err| ToolError::internal(format!("Failed to spawn command: {}", err)))?;
        let mut stdin_hold = None;
        if let Some(input) = stdin {
            if let Some(mut writer) = child.stdin.take() {
                match input {
                    StdinSource::Bytes(bytes) => {
                        writer.write_all(&bytes).await.map_err(|err| {
                            ToolError::internal(format!("Failed to write stdin: {}", err))
                        })?;
                    }
                    StdinSource::File(path) => {
                        let mut file = tokio::fs::File::open(&path).await.map_err(|err| {
                            ToolError::invalid_params(format!(
                                "stdin_file must be readable: {}",
                                err
                            ))
                        })?;
                        copy(&mut file, &mut writer).await.map_err(|err| {
                            ToolError::internal(format!("Failed to stream stdin file: {}", err))
                        })?;
                    }
                }
                if !stdin_eof {
                    stdin_hold = Some(writer);
                }
            }
        }

        let temp_dir = build_temp_dir();
        tokio::fs::create_dir_all(&temp_dir).await.ok();
        let token = random_token();
        let stdout_path = temp_dir.join(format!("stdout-{}.log", token));
        let stderr_path = temp_dir.join(format!("stderr-{}.log", token));

        let stdout_env = std::env::var("INFRA_LOCAL_EXEC_MAX_STDOUT_INLINE_BYTES").ok();
        let stderr_env = std::env::var("INFRA_LOCAL_EXEC_MAX_STDERR_INLINE_BYTES").ok();
        let stdout_override = stdout_env.as_ref().map(|s| Value::String(s.clone()));
        let stderr_override = stderr_env.as_ref().map(|s| Value::String(s.clone()));
        let max_stdout_inline = std::cmp::min(
            read_positive_int(stdout_override.as_ref()).unwrap_or(DEFAULT_STDOUT_INLINE_BYTES),
            256 * 1024,
        );
        let max_stderr_inline = std::cmp::min(
            read_positive_int(stderr_override.as_ref()).unwrap_or(DEFAULT_STDERR_INLINE_BYTES),
            256 * 1024,
        );

        let mut stdout_file = tokio::fs::File::create(&stdout_path)
            .await
            .map_err(|err| ToolError::internal(err.to_string()))?;
        let mut stderr_file = tokio::fs::File::create(&stderr_path)
            .await
            .map_err(|err| ToolError::internal(err.to_string()))?;

        let mut stdout_reader = child.stdout.take();
        let mut stderr_reader = child.stderr.take();

        let stdout_task = tokio::spawn(async move {
            let mut stdout_bytes: u64 = 0;
            let mut stdout_buf: Vec<u8> = Vec::new();
            let mut stdout_truncated = false;
            if let Some(mut reader) = stdout_reader.take() {
                let mut buf = [0u8; 8192];
                loop {
                    let n = match reader.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    stdout_bytes += n as u64;
                    let _ = stdout_file.write_all(&buf[..n]).await;
                    if inline && stdout_buf.len() < max_stdout_inline {
                        let remaining = max_stdout_inline - stdout_buf.len();
                        if n <= remaining {
                            stdout_buf.extend_from_slice(&buf[..n]);
                        } else {
                            stdout_buf.extend_from_slice(&buf[..remaining]);
                            stdout_truncated = true;
                        }
                    } else if inline {
                        stdout_truncated = true;
                    }
                }
            }
            (stdout_bytes, stdout_buf, stdout_truncated)
        });

        let stderr_task = tokio::spawn(async move {
            let mut stderr_bytes: u64 = 0;
            let mut stderr_buf: Vec<u8> = Vec::new();
            let mut stderr_truncated = false;
            if let Some(mut reader) = stderr_reader.take() {
                let mut buf = [0u8; 8192];
                loop {
                    let n = match reader.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    stderr_bytes += n as u64;
                    let _ = stderr_file.write_all(&buf[..n]).await;
                    if inline && stderr_buf.len() < max_stderr_inline {
                        let remaining = max_stderr_inline - stderr_buf.len();
                        if n <= remaining {
                            stderr_buf.extend_from_slice(&buf[..n]);
                        } else {
                            stderr_buf.extend_from_slice(&buf[..remaining]);
                            stderr_truncated = true;
                        }
                    } else if inline {
                        stderr_truncated = true;
                    }
                }
            }
            (stderr_bytes, stderr_buf, stderr_truncated)
        });

        let started = chrono::Utc::now().timestamp_millis();
        let mut timed_out = false;
        let status = if let Some(timeout) = timeout_ms {
            match tokio::time::timeout(std::time::Duration::from_millis(timeout), child.wait())
                .await
            {
                Ok(result) => result,
                Err(_) => {
                    timed_out = true;
                    let _ = child.kill().await;
                    child.wait().await
                }
            }
        } else {
            child.wait().await
        };
        let status = status
            .map_err(|err| ToolError::internal(format!("Failed to wait for process: {}", err)))?;
        drop(stdin_hold);

        let (stdout_bytes, stdout_buf, stdout_truncated) =
            stdout_task.await.unwrap_or((0, Vec::new(), false));
        let (stderr_bytes, stderr_buf, stderr_truncated) =
            stderr_task.await.unwrap_or((0, Vec::new(), false));

        let duration_ms = chrono::Utc::now().timestamp_millis() - started;
        let stdout_inline = if inline {
            String::from_utf8_lossy(&stdout_buf)
                .trim_end_matches(&['\r', '\n'][..])
                .to_string()
        } else {
            String::new()
        };
        let stderr_inline = if inline {
            String::from_utf8_lossy(&stderr_buf)
                .trim_end_matches(&['\r', '\n'][..])
                .to_string()
        } else {
            String::new()
        };

        Ok(serde_json::json!({
            "success": status.success(),
            "exit_code": status.code().unwrap_or(-1),
            "timed_out": timed_out,
            "duration_ms": duration_ms,
            "stdout": stdout_inline,
            "stderr": stderr_inline,
            "stdout_bytes": stdout_bytes,
            "stderr_bytes": stderr_bytes,
            "stdout_inline_truncated": stdout_truncated,
            "stderr_inline_truncated": stderr_truncated,
            "stdout_path": stdout_path,
            "stderr_path": stderr_path,
        }))
    }

    pub(super) async fn batch(&self, args: Value) -> Result<Value, ToolError> {
        let commands = args
            .get("commands")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if commands.is_empty() {
            return Err(ToolError::invalid_params("commands must be a non-empty array").with_hint(
                "Provide at least one command: { commands: [{ command: \"echo\", args: [\"hi\"] }] }."
                    .to_string(),
            ));
        }

        let parallel = args
            .get("parallel")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let stop_on_error = args
            .get("stop_on_error")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let base = args.as_object().cloned().unwrap_or_default();

        let run_one = |command: Value| async {
            let mut merged = base.clone();
            if let Some(obj) = command.as_object() {
                for (key, value) in obj {
                    merged.insert(key.clone(), value.clone());
                }
            }
            match self.exec(Value::Object(merged)).await {
                Ok(value) => Ok(value),
                Err(err) => Err((command, err)),
            }
        };

        if parallel {
            let joined = join_all(commands.into_iter().map(run_one)).await;
            let mut results = Vec::new();
            for item in joined {
                match item {
                    Ok(value) => results.push(value),
                    Err((command, err)) => {
                        let command_name = command
                            .get("command")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        results.push(serde_json::json!({
                            "success": false,
                            "command": command_name,
                            "error": err.message,
                        }));
                    }
                }
            }
            let success = results
                .iter()
                .all(|item| item.get("exit_code").and_then(|v| v.as_i64()) == Some(0));
            return Ok(serde_json::json!({ "success": success, "results": results }));
        }

        let mut results = Vec::new();
        for command in commands {
            match run_one(command.clone()).await {
                Ok(value) => {
                    let exit_code = value.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(1);
                    results.push(value);
                    if stop_on_error && exit_code != 0 {
                        break;
                    }
                }
                Err((failed, err)) => {
                    let command_name = failed.get("command").and_then(|v| v.as_str()).unwrap_or("");
                    results.push(serde_json::json!({
                        "success": false,
                        "command": command_name,
                        "error": err.message,
                    }));
                    if stop_on_error {
                        break;
                    }
                }
            }
        }

        let success = results
            .iter()
            .all(|item| item.get("exit_code").and_then(|v| v.as_i64()) == Some(0));
        Ok(serde_json::json!({ "success": success, "results": results }))
    }
}
