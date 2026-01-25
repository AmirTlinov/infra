use crate::constants::network as network_constants;
use crate::errors::ToolError;
use crate::services::job::JobService;
use crate::services::logger::Logger;
use crate::services::profile::ProfileService;
use crate::services::project_resolver::ProjectResolver;
use crate::services::secret_ref::SecretRefResolver;
use crate::services::security::Security;
use crate::services::validation::Validation;
use crate::utils::artifacts::{
    build_tool_call_file_ref, resolve_context_root, write_text_artifact,
};
use crate::utils::feature_flags::is_allow_secret_export_enabled;
use crate::utils::fs_atomic::{ensure_dir_for_file, temp_sibling_path};
use crate::utils::redact::redact_text;
use crate::utils::stdin::{resolve_stdin_source, StdinSource};
use crate::utils::tool_errors::unknown_action_error;
use crate::utils::user_paths::expand_home_path;
use base64::Engine;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use ssh2::{FileStat, OpenFlags, OpenType, Session};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SSH_PROFILE_TYPE: &str = "ssh";
const DEFAULT_MAX_CAPTURE_BYTES: usize = 256 * 1024;
const DEFAULT_MAX_INLINE_BYTES: usize = 16 * 1024;

const SSH_ACTIONS: &[&str] = &[
    "profile_upsert",
    "profile_get",
    "profile_list",
    "profile_delete",
    "profile_test",
    "connect",
    "authorized_keys_add",
    "exec",
    "exec_detached",
    "exec_follow",
    "deploy_file",
    "job_status",
    "job_wait",
    "job_logs_tail",
    "tail_job",
    "follow_job",
    "job_kill",
    "job_forget",
    "batch",
    "system_info",
    "check_host",
    "sftp_list",
    "sftp_exists",
    "sftp_upload",
    "sftp_download",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostKeyPolicy {
    Accept,
    Tofu,
    Pin,
}

#[derive(Clone, Debug)]
struct SshConnection {
    host: String,
    port: u16,
    username: String,
    password: Option<String>,
    private_key: Option<String>,
    passphrase: Option<String>,
    ready_timeout_ms: u64,
    keepalive_interval_ms: u64,
    host_key_policy: HostKeyPolicy,
    host_key_fingerprint: Option<String>,
}

#[derive(Clone, Debug)]
struct ResolvedConnection {
    connection: SshConnection,
    profile_name: Option<String>,
}

#[derive(Clone)]
pub struct SshManager {
    logger: Logger,
    security: Arc<Security>,
    validation: Validation,
    profile_service: Arc<ProfileService>,
    project_resolver: Option<Arc<ProjectResolver>>,
    secret_ref_resolver: Option<Arc<SecretRefResolver>>,
    job_service: Option<Arc<JobService>>,
    jobs: Arc<dashmap::DashMap<String, Value>>,
    max_jobs: usize,
}

impl SshManager {
    pub fn new(
        logger: Logger,
        security: Arc<Security>,
        validation: Validation,
        profile_service: Arc<ProfileService>,
        project_resolver: Option<Arc<ProjectResolver>>,
        secret_ref_resolver: Option<Arc<SecretRefResolver>>,
        job_service: Option<Arc<JobService>>,
    ) -> Self {
        let max_jobs = std::env::var("INFRA_SSH_MAX_JOBS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(200);
        Self {
            logger: logger.child("ssh"),
            security,
            validation,
            profile_service,
            project_resolver,
            secret_ref_resolver,
            job_service,
            jobs: Arc::new(dashmap::DashMap::new()),
            max_jobs,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "profile_upsert" => self.profile_upsert(&args).await,
            "profile_get" => self.profile_get(&args),
            "profile_list" => self.profile_list(),
            "profile_delete" => self.profile_delete(&args),
            "profile_test" | "connect" => self.profile_test(&args).await,
            "authorized_keys_add" => self.authorized_keys_add(&args).await,
            "exec" => self.exec_command(&args).await,
            "exec_detached" => self.exec_detached(&args).await,
            "exec_follow" => self.exec_follow(&args).await,
            "deploy_file" => self.deploy_file(&args).await,
            "job_status" => self.job_status(&args).await,
            "job_wait" => self.job_wait(&args).await,
            "job_logs_tail" => self.job_logs_tail(&args).await,
            "tail_job" => self.tail_job(&args).await,
            "follow_job" => self.follow_job(&args).await,
            "job_kill" => self.job_kill(&args).await,
            "job_forget" => self.job_forget(&args).await,
            "batch" => self.batch(&args).await,
            "system_info" => self.system_info(&args).await,
            "check_host" => self.check_host(&args).await,
            "sftp_list" => self.sftp_list(&args).await,
            "sftp_exists" => self.sftp_exists(&args).await,
            "sftp_upload" => self.sftp_upload(&args).await,
            "sftp_download" => self.sftp_download(&args).await,
            _ => Err(unknown_action_error("ssh", args.get("action"), SSH_ACTIONS)),
        }
    }

    pub async fn cleanup(&self) -> Result<Value, ToolError> {
        let cleared = self.jobs.len();
        self.jobs.clear();
        Ok(serde_json::json!({
            "success": true,
            "cleared_jobs": cleared,
        }))
    }

    async fn profile_upsert(&self, args: &Value) -> Result<Value, ToolError> {
        let name = self.validation.ensure_string(
            args.get("profile_name").unwrap_or(&Value::Null),
            "profile_name",
            true,
        )?;
        let connection = args
            .get("connection")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let secrets = serde_json::json!({
            "password": connection.get("password"),
            "private_key": connection.get("private_key"),
            "passphrase": connection.get("passphrase"),
        });
        let mut data = connection.clone();
        if let Some(obj) = data.as_object_mut() {
            obj.remove("password");
            obj.remove("private_key");
            obj.remove("passphrase");
        }

        self.profile_test(&serde_json::json!({"connection": connection}))
            .await?;
        let profile = self.profile_service.set_profile(
            &name,
            &serde_json::json!({
                "type": SSH_PROFILE_TYPE,
                "data": data,
                "secrets": secrets,
            }),
        )?;

        Ok(serde_json::json!({
            "success": true,
            "profile": {
                "name": name,
                "type": SSH_PROFILE_TYPE,
                "data": profile.get("data").cloned().unwrap_or(Value::Object(Default::default())),
                "auth": if secrets.get("private_key").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false) { "private_key" } else { "password" },
            }
        }))
    }

    fn profile_get(&self, args: &Value) -> Result<Value, ToolError> {
        let name = self.validation.ensure_string(
            args.get("profile_name").unwrap_or(&Value::Null),
            "profile_name",
            true,
        )?;
        let profile = self
            .profile_service
            .get_profile(&name, Some(SSH_PROFILE_TYPE))?;
        let include_secrets = args
            .get("include_secrets")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let allow = is_allow_secret_export_enabled();
        if include_secrets && allow {
            return Ok(serde_json::json!({"success": true, "profile": profile}));
        }
        let secret_keys = profile
            .get("secrets")
            .and_then(|v| v.as_object())
            .map(|map| map.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        Ok(serde_json::json!({
            "success": true,
            "profile": {
                "name": profile.get("name").cloned().unwrap_or(Value::String(name)),
                "type": profile.get("type").cloned().unwrap_or(Value::Null),
                "data": profile.get("data").cloned().unwrap_or(Value::Object(Default::default())),
                "secrets": secret_keys,
                "secrets_redacted": true,
            }
        }))
    }

    fn profile_list(&self) -> Result<Value, ToolError> {
        let profiles = self.profile_service.list_profiles(Some(SSH_PROFILE_TYPE))?;
        Ok(serde_json::json!({"success": true, "profiles": profiles}))
    }

    fn profile_delete(&self, args: &Value) -> Result<Value, ToolError> {
        let name = self.validation.ensure_string(
            args.get("profile_name").unwrap_or(&Value::Null),
            "profile_name",
            true,
        )?;
        self.profile_service.delete_profile(&name)
    }

    async fn profile_test(&self, args: &Value) -> Result<Value, ToolError> {
        let resolved = self.resolve_connection(args).await?;
        let connection = resolved.connection.clone();
        tokio::task::spawn_blocking(move || test_connection(&connection))
            .await
            .map_err(|_| ToolError::internal("SSH profile test task failed"))??;
        Ok(serde_json::json!({"success": true}))
    }

    async fn exec_command(&self, args: &Value) -> Result<Value, ToolError> {
        let raw_command = self.validation.ensure_string(
            args.get("command").unwrap_or(&Value::Null),
            "command",
            false,
        )?;
        let cwd = args.get("cwd").and_then(|v| v.as_str());
        let command = build_command(&self.security, &raw_command, cwd)?;

        let requested_timeout = read_positive_int(args.get("timeout_ms"));
        let budget_ms = resolve_tool_call_budget_ms();
        if requested_timeout.map(|v| v > budget_ms).unwrap_or(false) {
            let follow = self.exec_follow(args).await?;
            return Ok(serde_json::json!({
                "success": follow.get("success").cloned().unwrap_or(Value::Bool(false)),
                "detached": true,
                "requested_timeout_ms": requested_timeout,
                "command": command,
                "follow": follow,
            }));
        }

        let timeout_ms = std::cmp::min(
            requested_timeout.unwrap_or(resolve_exec_default_timeout_ms()),
            budget_ms,
        );

        self.exec_command_once(args, command, timeout_ms, requested_timeout)
            .await
    }

    async fn exec_command_once(
        &self,
        args: &Value,
        command: String,
        timeout_ms: u64,
        requested_timeout: Option<u64>,
    ) -> Result<Value, ToolError> {
        let resolved = self.resolve_connection(args).await?;
        let env = args.get("env").cloned();
        let pty = args.get("pty").and_then(|v| v.as_bool()).unwrap_or(false);
        let stdin = resolve_stdin_source(args)?;
        let stdin_eof = args
            .get("stdin_eof")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let trace_id = args
            .get("trace_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let span_id = args
            .get("span_id")
            .or_else(|| args.get("parent_span_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let command_clone = command.clone();
        let resolved_clone = resolved.clone();
        let profile_service = self.profile_service.clone();
        let result = tokio::task::spawn_blocking(move || {
            exec_blocking(
                &resolved_clone,
                profile_service,
                &command_clone,
                env,
                pty,
                stdin,
                stdin_eof,
                Some(timeout_ms),
                trace_id,
                span_id,
            )
        })
        .await
        .map_err(|_| ToolError::internal("SSH exec task failed"))??;

        Ok(serde_json::json!({
            "success": result.exit_code == 0 && !result.timed_out,
            "command": command,
            "timeout_ms": timeout_ms,
            "requested_timeout_ms": requested_timeout,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "stdout_bytes": result.stdout_bytes,
            "stderr_bytes": result.stderr_bytes,
            "stdout_captured_bytes": result.stdout_captured_bytes,
            "stderr_captured_bytes": result.stderr_captured_bytes,
            "stdout_truncated": result.stdout_truncated,
            "stderr_truncated": result.stderr_truncated,
            "stdout_inline_truncated": result.stdout_inline_truncated,
            "stderr_inline_truncated": result.stderr_inline_truncated,
            "stdout_ref": result.stdout_ref,
            "stderr_ref": result.stderr_ref,
            "exitCode": result.exit_code,
            "signal": result.signal,
            "timedOut": result.timed_out,
            "hardTimedOut": result.hard_timed_out,
            "duration_ms": result.duration_ms,
        }))
    }

    async fn exec_detached(&self, args: &Value) -> Result<Value, ToolError> {
        let raw_command = self.validation.ensure_string(
            args.get("command").unwrap_or(&Value::Null),
            "command",
            false,
        )?;
        let cwd = args.get("cwd").and_then(|v| v.as_str());
        let command = build_command(&self.security, &raw_command, cwd)?;

        let start_timeout_ms = std::cmp::min(
            read_positive_int(args.get("timeout_ms"))
                .unwrap_or(resolve_detached_start_timeout_ms()),
            resolve_tool_call_budget_ms(),
        );
        let stdin_source = resolve_stdin_source(args)?;
        let stdin_path = if let Some(source) = stdin_source.as_ref() {
            let path = format!("/tmp/infra-stdin-{}.txt", uuid::Uuid::new_v4());
            let upload_command = format!("cat > {}", escape_shell_value(&path));
            let mut upload_args = args.clone();
            if let Value::Object(map) = &mut upload_args {
                map.insert("command".to_string(), Value::String(upload_command.clone()));
                map.insert("cwd".to_string(), Value::Null);
                map.insert("pty".to_string(), Value::Bool(false));
                map.insert(
                    "timeout_ms".to_string(),
                    Value::Number(start_timeout_ms.into()),
                );
                map.insert("stdin_eof".to_string(), Value::Bool(true));
                crate::utils::stdin::apply_stdin_source(map, source);
            }
            self.exec_command_once(
                &upload_args,
                upload_command.clone(),
                start_timeout_ms,
                Some(start_timeout_ms),
            )
            .await?;
            Some(path)
        } else {
            None
        };

        let log_path = args
            .get("log_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                format!(
                    "/tmp/infra-detached-{}-{}.log",
                    chrono::Utc::now().timestamp_millis(),
                    rand::random::<u32>()
                )
            });
        let pid_path = args
            .get("pid_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{}.pid", log_path));
        let exit_path = args
            .get("exit_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{}.exit", log_path));

        let job_id = uuid::Uuid::new_v4().to_string();
        let inner_body = if let Some(path) = stdin_path.as_ref() {
            format!("({}) < {}", command, escape_shell_value(path))
        } else {
            format!("({})", command)
        };
        let inner = if let Some(path) = stdin_path.as_ref() {
            format!(
                "{body}\nrc=$?\nrm -f {stdin}\necho \"$rc\" > {exit}\nexit \"$rc\"",
                body = inner_body,
                stdin = escape_shell_value(path),
                exit = escape_shell_value(&exit_path)
            )
        } else {
            format!(
                "{body}\nrc=$?\necho \"$rc\" > {exit}\nexit \"$rc\"",
                body = inner_body,
                exit = escape_shell_value(&exit_path)
            )
        };
        let detached_command = format!(
            "rm -f {pid} {exit} 2>/dev/null || true; nohup sh -lc {inner} > {log} 2>&1 < /dev/null & echo $! > {pid}; cat {pid}",
            pid = escape_shell_value(&pid_path),
            exit = escape_shell_value(&exit_path),
            inner = escape_shell_value(&inner),
            log = escape_shell_value(&log_path)
        );

        let exec_args = serde_json::json!({
            "command": detached_command,
            "cwd": Value::Null,
            "pty": false,
            "timeout_ms": start_timeout_ms,
        });
        let mut merged = args.clone();
        if let Value::Object(map) = &mut merged {
            map.extend(exec_args.as_object().cloned().unwrap_or_default());
            map.remove("stdin");
            map.remove("stdin_base64");
            map.remove("stdin_file");
            map.remove("stdin_ref");
            map.remove("stdin_eof");
        }
        let exec = self
            .exec_command_once(
                &merged,
                detached_command.clone(),
                start_timeout_ms,
                Some(start_timeout_ms),
            )
            .await?;
        let stdout = exec.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
        let pid = stdout
            .split_whitespace()
            .last()
            .and_then(|s| s.parse::<i64>().ok());

        self.register_job(serde_json::json!({
            "job_id": job_id,
            "created_at": chrono::Utc::now().to_rfc3339(),
            "profile_name": merged.get("profile_name").cloned().unwrap_or(Value::Null),
            "pid": pid,
            "log_path": log_path,
            "pid_path": pid_path,
            "exit_path": exit_path,
        }));

        Ok(serde_json::json!({
            "success": exec.get("exitCode").and_then(|v| v.as_i64()) == Some(0) && pid.is_some(),
            "job_id": job_id,
            "command": command,
            "detached_command": detached_command,
            "pid": pid,
            "log_path": log_path,
            "pid_path": pid_path,
            "exit_path": exit_path,
            "start_timeout_ms": start_timeout_ms,
            "stdout": exec.get("stdout").cloned().unwrap_or(Value::Null),
            "stderr": exec.get("stderr").cloned().unwrap_or(Value::Null),
            "exitCode": exec.get("exitCode").cloned().unwrap_or(Value::Null),
        }))
    }

    async fn exec_follow(&self, args: &Value) -> Result<Value, ToolError> {
        let started_at = Instant::now();
        let budget_ms = resolve_tool_call_budget_ms();
        let start_timeout_ms = std::cmp::min(
            read_positive_int(args.get("start_timeout_ms"))
                .unwrap_or(resolve_detached_start_timeout_ms()),
            budget_ms,
        );

        let mut start_args = args.clone();
        if let Value::Object(map) = &mut start_args {
            map.insert(
                "timeout_ms".to_string(),
                Value::Number(start_timeout_ms.into()),
            );
        }
        let started = self.exec_detached(&start_args).await?;
        let job_id = started
            .get("job_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if job_id.trim().is_empty()
            || started.get("success").and_then(|v| v.as_bool()) != Some(true)
        {
            return Ok(serde_json::json!({
                "success": false,
                "code": "START_FAILED",
                "job_id": if job_id.is_empty() { Value::Null } else { Value::String(job_id) },
                "start": started,
            }));
        }

        let elapsed = started_at.elapsed().as_millis() as u64;
        let remaining = budget_ms.saturating_sub(elapsed);
        let requested_wait = read_positive_int(args.get("timeout_ms")).unwrap_or(30_000);
        let wait_timeout_ms = std::cmp::max(1, std::cmp::min(requested_wait, remaining));

        let mut follow_args = args.clone();
        if let Value::Object(map) = &mut follow_args {
            map.insert("job_id".to_string(), Value::String(job_id.clone()));
            map.insert(
                "timeout_ms".to_string(),
                Value::Number(wait_timeout_ms.into()),
            );
        }
        let follow = self.follow_job(&follow_args).await?;

        Ok(serde_json::json!({
            "success": follow.get("success").and_then(|v| v.as_bool()).unwrap_or(false),
            "job_id": job_id,
            "start": {
                "success": true,
                "pid": started.get("pid").cloned().unwrap_or(Value::Null),
                "log_path": started.get("log_path").cloned().unwrap_or(Value::Null),
                "pid_path": started.get("pid_path").cloned().unwrap_or(Value::Null),
                "exit_path": started.get("exit_path").cloned().unwrap_or(Value::Null),
                "start_timeout_ms": started.get("start_timeout_ms").cloned().unwrap_or(Value::Number(start_timeout_ms.into())),
            },
            "wait": follow.get("wait").cloned().unwrap_or(Value::Null),
            "status": follow.get("status").cloned().unwrap_or(Value::Null),
            "logs": follow.get("logs").cloned().unwrap_or(Value::Null),
        }))
    }

    async fn authorized_keys_add(&self, args: &Value) -> Result<Value, ToolError> {
        let public_key_line = resolve_public_key_line(args)?;
        let (key_type, _key_blob) = parse_public_key_tokens(&public_key_line)?;
        let fingerprint = fingerprint_public_key_sha256(&public_key_line)?;

        let authorized_keys_path = args
            .get("authorized_keys_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let script = [
            "set -eu",
            "umask 077",
            "auth_path=\"${AUTH_KEYS_PATH:-\"$HOME/.ssh/authorized_keys\"}\"",
            "ssh_dir=\"${auth_path%/*}\"",
            "mkdir -p \"$ssh_dir\"",
            "chmod 700 \"$ssh_dir\" 2>/dev/null || true",
            "[ -f \"$auth_path\" ] || : > \"$auth_path\"",
            "chmod 600 \"$auth_path\" 2>/dev/null || true",
            "IFS= read -r key_line",
            "key_line=\"$(printf %s \"$key_line\" | tr -d '\\r')\"",
            "set -- $key_line",
            "key_type=\"${1:-}\"",
            "key_blob=\"${2:-}\"",
            "[ -n \"$key_type\" ] && [ -n \"$key_blob\" ] || { echo \"invalid_key\" >&2; exit 2; }",
            "if awk -v t=\"$key_type\" -v b=\"$key_blob\" '$0 ~ /^[[:space:]]*#/ { next } { for (i = 1; i <= NF; i++) if ($i == t && (i + 1) <= NF && $(i+1) == b) { found = 1; exit } } END { exit found ? 0 : 1 }' \"$auth_path\"; then",
            "  echo present",
            "else",
            "  printf \"%s\\n\" \"$key_line\" >> \"$auth_path\"",
            "  echo added",
            "fi",
        ]
        .join("\n");

        let mut exec_args = args.clone();
        if let Value::Object(map) = &mut exec_args {
            if let Some(path) = authorized_keys_path.clone() {
                let mut env = map
                    .get("env")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                if let Value::Object(env_map) = &mut env {
                    env_map.insert("AUTH_KEYS_PATH".to_string(), Value::String(path));
                }
                map.insert("env".to_string(), env);
            }
            map.insert("command".to_string(), Value::String(script));
            map.insert(
                "stdin".to_string(),
                Value::String(format!("{}\n", public_key_line)),
            );
            map.insert("pty".to_string(), Value::Bool(false));
        }

        let result = self.exec_command(&exec_args).await?;
        let marker = result
            .get("stdout")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .split('\n')
            .next_back()
            .unwrap_or("")
            .to_string();
        let exit_code = result
            .get("exitCode")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        if exit_code != 0 {
            return Err(ToolError::internal(format!(
                "authorized_keys_add failed: {}",
                result
                    .get("stderr")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error")
            )));
        }

        Ok(serde_json::json!({
            "success": marker == "added" || marker == "present",
            "changed": marker == "added",
            "key_type": key_type,
            "key_fingerprint_sha256": fingerprint,
            "authorized_keys_path": authorized_keys_path.unwrap_or_else(|| "~/.ssh/authorized_keys".to_string()),
        }))
    }

    async fn deploy_file(&self, args: &Value) -> Result<Value, ToolError> {
        let started = Instant::now();
        let local_path = expand_home_path(self.validation.ensure_string(
            args.get("local_path").unwrap_or(&Value::Null),
            "local_path",
            true,
        )?);
        let remote_path = self.validation.ensure_string(
            args.get("remote_path").unwrap_or(&Value::Null),
            "remote_path",
            true,
        )?;

        let overwrite = args
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let mkdirs = args
            .get("mkdirs")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let preserve_mtime = args
            .get("preserve_mtime")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let local_sha256 = compute_local_sha256_hex(&local_path)?;

        let upload = self
            .sftp_upload(&serde_json::json!({
                "profile_name": args.get("profile_name"),
                "connection": args.get("connection"),
                "local_path": local_path.display().to_string(),
                "remote_path": remote_path,
                "overwrite": overwrite,
                "mkdirs": mkdirs,
                "preserve_mtime": preserve_mtime,
            }))
            .await;
        if let Err(err) = upload {
            return Ok(serde_json::json!({
                "success": false,
                "code": "UPLOAD_FAILED",
                "local_path": local_path.display().to_string(),
                "remote_path": remote_path,
                "local_sha256": local_sha256,
                "error": err.message,
                "duration_ms": started.elapsed().as_millis(),
            }));
        }

        let hash_cmd = build_remote_sha256_command(&remote_path);
        let mut exec_args = args.clone();
        if let Value::Object(map) = &mut exec_args {
            map.insert("command".to_string(), Value::String(hash_cmd));
            map.insert("pty".to_string(), Value::Bool(false));
        }
        let hash_exec = self.exec_command(&exec_args).await?;
        let remote_sha256 = hash_exec
            .get("stdout")
            .and_then(|v| v.as_str())
            .and_then(parse_sha256_from_output);

        if remote_sha256.is_none() {
            return Ok(serde_json::json!({
                "success": false,
                "code": "REMOTE_HASH_FAILED",
                "local_path": local_path.display().to_string(),
                "remote_path": remote_path,
                "local_sha256": local_sha256,
                "remote_sha256": Value::Null,
                "error": "Unable to parse remote sha256 output",
                "remote_stdout": hash_exec.get("stdout").cloned().unwrap_or(Value::Null),
                "remote_stderr": hash_exec.get("stderr").cloned().unwrap_or(Value::Null),
                "remote_exit_code": hash_exec.get("exitCode").cloned().unwrap_or(Value::Null),
                "duration_ms": started.elapsed().as_millis(),
            }));
        }
        let remote_sha256 = remote_sha256.unwrap();
        if remote_sha256 != local_sha256 {
            return Ok(serde_json::json!({
                "success": false,
                "code": "HASH_MISMATCH",
                "local_path": local_path.display().to_string(),
                "remote_path": remote_path,
                "local_sha256": local_sha256,
                "remote_sha256": remote_sha256,
                "duration_ms": started.elapsed().as_millis(),
            }));
        }

        let restart_service = args
            .get("restart")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let restart_command = args
            .get("restart_command")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if restart_service.is_some() && restart_command.is_some() {
            return Ok(serde_json::json!({
                "success": false,
                "code": "INVALID_RESTART",
                "message": "Provide only one of restart (service) or restart_command",
                "local_path": local_path.display().to_string(),
                "remote_path": remote_path,
                "local_sha256": local_sha256,
                "remote_sha256": remote_sha256,
                "duration_ms": started.elapsed().as_millis(),
            }));
        }

        let mut restart_result = Value::Null;
        if restart_service.is_some() || restart_command.is_some() {
            let restart_started = Instant::now();
            let cmd = restart_command.unwrap_or_else(|| {
                format!(
                    "systemctl restart {} && systemctl is-active {}",
                    escape_shell_value(restart_service.as_ref().unwrap()),
                    escape_shell_value(restart_service.as_ref().unwrap())
                )
            });
            let mut restart_args = args.clone();
            if let Value::Object(map) = &mut restart_args {
                map.insert("command".to_string(), Value::String(cmd));
                map.insert("pty".to_string(), Value::Bool(false));
            }
            let out = self.exec_command(&restart_args).await?;
            let exit_code = out.get("exitCode").and_then(|v| v.as_i64());
            let timed_out = out
                .get("timedOut")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || out
                    .get("hardTimedOut")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            restart_result = serde_json::json!({
                "requested": true,
                "service": restart_service,
                "exit_code": exit_code,
                "timed_out": timed_out,
                "restart_ms": restart_started.elapsed().as_millis(),
            });
            if exit_code.unwrap_or(-1) != 0 || timed_out {
                return Ok(serde_json::json!({
                    "success": false,
                    "code": "RESTART_FAILED",
                    "local_path": local_path.display().to_string(),
                    "remote_path": remote_path,
                    "local_sha256": local_sha256,
                    "remote_sha256": remote_sha256,
                    "restart": restart_result,
                    "duration_ms": started.elapsed().as_millis(),
                }));
            }
        }

        Ok(serde_json::json!({
            "success": true,
            "local_path": local_path.display().to_string(),
            "remote_path": remote_path,
            "overwrite": overwrite,
            "mkdirs": mkdirs,
            "preserve_mtime": preserve_mtime,
            "local_sha256": local_sha256,
            "remote_sha256": remote_sha256,
            "verified": true,
            "restart": restart_result,
            "duration_ms": started.elapsed().as_millis(),
        }))
    }

    async fn job_status(&self, args: &Value) -> Result<Value, ToolError> {
        let spec = self.resolve_job_spec(args, false)?;
        if spec.not_found {
            return Ok(
                serde_json::json!({"success": false, "code": "NOT_FOUND", "job_id": spec.job_id}),
            );
        }
        let budget_ms = resolve_tool_call_budget_ms();
        let timeout_ms = std::cmp::min(
            read_positive_int(args.get("timeout_ms")).unwrap_or(10_000),
            budget_ms,
        );

        let pid_value = spec.pid.map(|v| v.to_string()).unwrap_or_default();
        let pid_path = spec.pid_path.clone().unwrap_or_default();
        let exit_path = spec.exit_path.clone().unwrap_or_default();
        let log_path = spec.log_path.clone().unwrap_or_default();

        let script = [
            "set -u",
            &format!("PID_VALUE={}", escape_shell_value(&pid_value)),
            &format!("PID_PATH={}", escape_shell_value(&pid_path)),
            &format!("EXIT_PATH={}", escape_shell_value(&exit_path)),
            &format!("LOG_PATH={}", escape_shell_value(&log_path)),
            "pid=\"$PID_VALUE\"",
            "if [ -z \"$pid\" ] && [ -n \"$PID_PATH\" ] && [ -f \"$PID_PATH\" ]; then pid=\"$(cat \"$PID_PATH\" 2>/dev/null | tr -dc '0-9' | head -c 32)\"; fi",
            "running=0",
            "if [ -n \"$pid\" ] && kill -0 \"$pid\" 2>/dev/null; then running=1; fi",
            "exit_code=\"\"",
            "if [ -n \"$EXIT_PATH\" ] && [ -f \"$EXIT_PATH\" ]; then exit_code=\"$(cat \"$EXIT_PATH\" 2>/dev/null | tr -d '\\r\\n' | head -c 64)\"; fi",
            "log_bytes=\"\"",
            "if [ -n \"$LOG_PATH\" ] && [ -f \"$LOG_PATH\" ]; then log_bytes=\"$(wc -c < \"$LOG_PATH\" 2>/dev/null | tr -d ' ')\"; fi",
            "echo \"__INFRA_PID__=$pid\"",
            "echo \"__INFRA_RUNNING__=$running\"",
            "echo \"__INFRA_EXIT_CODE__=$exit_code\"",
            "echo \"__INFRA_LOG_BYTES__=$log_bytes\"",
        ]
        .join("\n");
        let script_for_exec = script.clone();

        let mut exec_args = args.clone();
        if let Value::Object(map) = &mut exec_args {
            if let Some(profile) = spec.profile_name.clone() {
                map.insert("profile_name".to_string(), Value::String(profile));
            }
            map.insert("command".to_string(), Value::String(script));
            map.insert("timeout_ms".to_string(), Value::Number(timeout_ms.into()));
            map.insert("pty".to_string(), Value::Bool(false));
        }
        let exec = self
            .exec_command_once(&exec_args, script_for_exec, timeout_ms, Some(timeout_ms))
            .await?;
        let stdout = exec.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
        let lines: Vec<&str> = stdout.lines().collect();
        let pick = |prefix: &str| -> String {
            lines
                .iter()
                .find(|line| line.starts_with(prefix))
                .map(|line| line.trim_start_matches(prefix).to_string())
                .unwrap_or_default()
        };

        let pid_str = pick("__INFRA_PID__=");
        let running_str = pick("__INFRA_RUNNING__=");
        let exit_str = pick("__INFRA_EXIT_CODE__=");
        let log_bytes_str = pick("__INFRA_LOG_BYTES__=");

        let resolved_pid = pid_str.parse::<i64>().ok();
        let running = running_str == "1";
        let exit_code = exit_str.parse::<i64>().ok();
        let exited = !exit_str.trim().is_empty() && exit_code.is_some();
        let log_bytes = log_bytes_str.parse::<i64>().ok();

        Ok(serde_json::json!({
            "success": true,
            "job_id": spec.job_id,
            "pid": resolved_pid.or(spec.pid),
            "running": running,
            "exited": exited,
            "exit_code": if exited { exit_code } else { None },
            "log_path": spec.log_path,
            "pid_path": spec.pid_path,
            "exit_path": spec.exit_path,
            "log_bytes": log_bytes,
        }))
    }

    async fn job_wait(&self, args: &Value) -> Result<Value, ToolError> {
        let budget_ms = resolve_tool_call_budget_ms();
        let requested = read_positive_int(args.get("timeout_ms")).unwrap_or(30_000);
        let timeout_ms = std::cmp::min(requested, budget_ms);
        let poll_ms = std::cmp::min(
            read_positive_int(args.get("poll_interval_ms")).unwrap_or(1000),
            5000,
        );
        let started = Instant::now();

        let mut status = self
            .job_status(&serde_json::json!({
                "job_id": args.get("job_id").cloned().unwrap_or(Value::Null),
                "pid": args.get("pid").cloned().unwrap_or(Value::Null),
                "pid_path": args.get("pid_path").cloned().unwrap_or(Value::Null),
                "log_path": args.get("log_path").cloned().unwrap_or(Value::Null),
                "exit_path": args.get("exit_path").cloned().unwrap_or(Value::Null),
                "profile_name": args.get("profile_name").cloned().unwrap_or(Value::Null),
                "timeout_ms": std::cmp::min(10_000, budget_ms),
            }))
            .await?;
        if status.get("success").and_then(|v| v.as_bool()) == Some(false)
            && status.get("code").and_then(|v| v.as_str()) == Some("NOT_FOUND")
        {
            return Ok(
                serde_json::json!({"success": false, "code": "NOT_FOUND", "job_id": args.get("job_id").cloned().unwrap_or(Value::Null)}),
            );
        }
        while status.get("exited").and_then(|v| v.as_bool()) != Some(true)
            && started.elapsed().as_millis() as u64 + poll_ms <= timeout_ms
        {
            tokio::time::sleep(Duration::from_millis(poll_ms)).await;
            status = self.job_status(args).await?;
        }
        let waited_ms = started.elapsed().as_millis() as u64;
        Ok(serde_json::json!({
            "success": true,
            "completed": status.get("exited").and_then(|v| v.as_bool()).unwrap_or(false),
            "timed_out": status.get("exited").and_then(|v| v.as_bool()) != Some(true),
            "waited_ms": waited_ms,
            "timeout_ms": timeout_ms,
            "poll_interval_ms": poll_ms,
            "status": status,
        }))
    }

    async fn job_logs_tail(&self, args: &Value) -> Result<Value, ToolError> {
        let spec = self.resolve_job_spec(args, true)?;
        if spec.not_found {
            return Ok(
                serde_json::json!({"success": false, "code": "NOT_FOUND", "job_id": spec.job_id}),
            );
        }
        let log_path = spec
            .log_path
            .clone()
            .ok_or_else(|| ToolError::invalid_params("log_path is required"))?;
        let lines = std::cmp::min(read_positive_int(args.get("lines")).unwrap_or(200), 2000);
        let budget_ms = resolve_tool_call_budget_ms();
        let timeout_ms = std::cmp::min(
            read_positive_int(args.get("timeout_ms")).unwrap_or(10_000),
            budget_ms,
        );
        let cmd = format!(
            "tail -n {} {} 2>/dev/null || true",
            lines,
            escape_shell_value(&log_path)
        );
        let cmd_for_exec = cmd.clone();
        let mut exec_args = args.clone();
        if let Value::Object(map) = &mut exec_args {
            if let Some(profile) = spec.profile_name.clone() {
                map.insert("profile_name".to_string(), Value::String(profile));
            }
            map.insert("command".to_string(), Value::String(cmd));
            map.insert("timeout_ms".to_string(), Value::Number(timeout_ms.into()));
            map.insert("pty".to_string(), Value::Bool(false));
        }
        let out = self
            .exec_command_once(&exec_args, cmd_for_exec, timeout_ms, Some(timeout_ms))
            .await?;
        Ok(serde_json::json!({
            "success": true,
            "job_id": spec.job_id,
            "log_path": log_path,
            "lines": lines,
            "text": out.get("stdout").cloned().unwrap_or(Value::String("".to_string())),
        }))
    }

    async fn tail_job(&self, args: &Value) -> Result<Value, ToolError> {
        let lines = std::cmp::min(read_positive_int(args.get("lines")).unwrap_or(120), 2000);
        let budget_ms = resolve_tool_call_budget_ms();
        let status = self
            .job_status(&serde_json::json!({
                "job_id": args.get("job_id").cloned().unwrap_or(Value::Null),
                "pid": args.get("pid").cloned().unwrap_or(Value::Null),
                "pid_path": args.get("pid_path").cloned().unwrap_or(Value::Null),
                "log_path": args.get("log_path").cloned().unwrap_or(Value::Null),
                "exit_path": args.get("exit_path").cloned().unwrap_or(Value::Null),
                "profile_name": args.get("profile_name").cloned().unwrap_or(Value::Null),
                "timeout_ms": std::cmp::min(10_000, budget_ms),
            }))
            .await?;
        let logs = self
            .job_logs_tail(&serde_json::json!({
                "job_id": args.get("job_id").cloned().unwrap_or(Value::Null),
                "pid": args.get("pid").cloned().unwrap_or(Value::Null),
                "pid_path": args.get("pid_path").cloned().unwrap_or(Value::Null),
                "log_path": args.get("log_path").cloned().unwrap_or(Value::Null),
                "exit_path": args.get("exit_path").cloned().unwrap_or(Value::Null),
                "profile_name": args.get("profile_name").cloned().unwrap_or(Value::Null),
                "lines": lines,
                "timeout_ms": std::cmp::min(10_000, budget_ms),
            }))
            .await?;
        Ok(serde_json::json!({
            "success": true,
            "status": status,
            "logs": logs,
        }))
    }

    async fn follow_job(&self, args: &Value) -> Result<Value, ToolError> {
        let budget_ms = resolve_tool_call_budget_ms();
        let requested = read_positive_int(args.get("timeout_ms")).unwrap_or(30_000);
        let wait_timeout_ms = std::cmp::min(requested, budget_ms);
        let wait = self
            .job_wait(&serde_json::json!({
                "job_id": args.get("job_id").cloned().unwrap_or(Value::Null),
                "pid": args.get("pid").cloned().unwrap_or(Value::Null),
                "pid_path": args.get("pid_path").cloned().unwrap_or(Value::Null),
                "log_path": args.get("log_path").cloned().unwrap_or(Value::Null),
                "exit_path": args.get("exit_path").cloned().unwrap_or(Value::Null),
                "profile_name": args.get("profile_name").cloned().unwrap_or(Value::Null),
                "timeout_ms": wait_timeout_ms,
            }))
            .await?;
        let lines = std::cmp::min(read_positive_int(args.get("lines")).unwrap_or(200), 2000);
        let logs = self
            .job_logs_tail(&serde_json::json!({
                "job_id": args.get("job_id").cloned().unwrap_or(Value::Null),
                "pid": args.get("pid").cloned().unwrap_or(Value::Null),
                "pid_path": args.get("pid_path").cloned().unwrap_or(Value::Null),
                "log_path": args.get("log_path").cloned().unwrap_or(Value::Null),
                "exit_path": args.get("exit_path").cloned().unwrap_or(Value::Null),
                "profile_name": args.get("profile_name").cloned().unwrap_or(Value::Null),
                "lines": lines,
                "timeout_ms": std::cmp::min(10_000, budget_ms),
            }))
            .await?;
        Ok(serde_json::json!({
            "success": true,
            "wait": wait,
            "status": wait.get("status").cloned().unwrap_or(Value::Null),
            "logs": logs,
        }))
    }

    async fn job_kill(&self, args: &Value) -> Result<Value, ToolError> {
        let spec = self.resolve_job_spec(args, false)?;
        if spec.not_found {
            return Ok(
                serde_json::json!({"success": false, "code": "NOT_FOUND", "job_id": spec.job_id}),
            );
        }
        let pid = spec
            .pid
            .ok_or_else(|| ToolError::invalid_params("job requires pid"))?;
        let cmd = format!("kill {} 2>/dev/null || true", pid);
        let mut exec_args = args.clone();
        if let Value::Object(map) = &mut exec_args {
            if let Some(profile) = spec.profile_name.clone() {
                map.insert("profile_name".to_string(), Value::String(profile));
            }
            map.insert("command".to_string(), Value::String(cmd));
            map.insert("pty".to_string(), Value::Bool(false));
        }
        let _ = self.exec_command(&exec_args).await?;
        Ok(serde_json::json!({"success": true, "job_id": spec.job_id, "pid": pid}))
    }

    async fn job_forget(&self, args: &Value) -> Result<Value, ToolError> {
        let job_id = args.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
        if job_id.trim().is_empty() {
            return Err(ToolError::invalid_params("job_id is required"));
        }
        if let Some(service) = &self.job_service {
            service.forget(job_id);
        } else {
            self.jobs.remove(job_id);
        }
        Ok(serde_json::json!({"success": true, "job_id": job_id}))
    }

    async fn batch(&self, args: &Value) -> Result<Value, ToolError> {
        let commands = args
            .get("commands")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if commands.is_empty() {
            return Err(
                ToolError::invalid_params("commands must be a non-empty array")
                    .with_hint("Example: { action: 'batch', commands: [{ command: 'uname -a' }] }"),
            );
        }
        let stop_on_error = args
            .get("stop_on_error")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let mut results = Vec::new();
        for command in commands {
            if let Some(cmd_obj) = command.as_object() {
                let mut merged = args.clone();
                if let Value::Object(map) = &mut merged {
                    for (k, v) in cmd_obj {
                        map.insert(k.clone(), v.clone());
                    }
                }
                match self.exec_command(&merged).await {
                    Ok(result) => {
                        let exit_code = result
                            .get("exitCode")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(-1);
                        results.push(result);
                        if stop_on_error && exit_code != 0 {
                            break;
                        }
                    }
                    Err(err) => {
                        results.push(serde_json::json!({"success": false, "error": err.message}));
                        if stop_on_error {
                            break;
                        }
                    }
                }
            }
        }
        Ok(serde_json::json!({
            "success": results.iter().all(|v| v.get("exitCode").and_then(|v| v.as_i64()).unwrap_or(0) == 0),
            "results": results,
        }))
    }

    async fn system_info(&self, args: &Value) -> Result<Value, ToolError> {
        let commands = [
            ("uname", "uname -a"),
            ("os", "cat /etc/os-release 2>/dev/null || sw_vers 2>/dev/null || echo \"OS info unavailable\""),
            ("disk", "df -h"),
            ("memory", "free -h 2>/dev/null || vm_stat"),
            ("uptime", "uptime"),
        ];
        let mut report = serde_json::Map::new();
        for (key, cmd) in commands {
            let mut exec_args = args.clone();
            if let Value::Object(map) = &mut exec_args {
                map.insert("command".to_string(), Value::String(cmd.to_string()));
            }
            let entry = match self.exec_command(&exec_args).await {
                Ok(result) => {
                    let mut obj = serde_json::Map::new();
                    obj.insert("success".to_string(), Value::Bool(true));
                    if let Some(obj_result) = result.as_object() {
                        for (k, v) in obj_result {
                            obj.insert(k.clone(), v.clone());
                        }
                    }
                    Value::Object(obj)
                }
                Err(err) => serde_json::json!({"success": false, "error": err.message}),
            };
            report.insert(key.to_string(), entry);
        }
        Ok(serde_json::json!({"success": true, "system_info": report}))
    }

    async fn check_host(&self, args: &Value) -> Result<Value, ToolError> {
        let mut exec_args = args.clone();
        if let Value::Object(map) = &mut exec_args {
            map.insert(
                "command".to_string(),
                Value::String("echo \"Connection OK\" && whoami && hostname".to_string()),
            );
        }
        match self.exec_command(&exec_args).await {
            Ok(result) => Ok(serde_json::json!({
                "success": result.get("exitCode").and_then(|v| v.as_i64()) == Some(0),
                "response": result.get("stdout").cloned().unwrap_or(Value::Null),
            })),
            Err(err) => Ok(serde_json::json!({"success": false, "error": err.message})),
        }
    }

    async fn sftp_list(&self, args: &Value) -> Result<Value, ToolError> {
        let remote_path = self.validation.ensure_string(
            args.get("path")
                .or_else(|| args.get("remote_path"))
                .unwrap_or(&Value::Null),
            "path",
            true,
        )?;
        let recursive = args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max_depth = args.get("max_depth").and_then(|v| v.as_i64()).unwrap_or(3) as i32;

        let remote_path_clone = remote_path.clone();
        let entries = self
            .with_sftp(args, move |sftp| {
                let mut out = Vec::new();
                fn walk(
                    sftp: &ssh2::Sftp,
                    current: &str,
                    depth: i32,
                    max_depth: i32,
                    recursive: bool,
                    out: &mut Vec<Value>,
                ) -> Result<(), ToolError> {
                    let list = sftp.readdir(Path::new(current)).map_err(map_ssh_error)?;
                    for (path, stat) in list {
                        let filename = path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        let full_path = path.to_string_lossy().to_string();
                        let is_dir = stat.perm.map(|p| p & 0o40000 != 0).unwrap_or(false);
                        out.push(serde_json::json!({
                            "path": full_path,
                            "filename": filename,
                            "type": if is_dir { "dir" } else { "file" },
                            "size": stat.size,
                            "mode": stat.perm,
                            "mtime": stat.mtime,
                            "atime": stat.atime,
                        }));
                        if recursive && is_dir && depth < max_depth {
                            walk(
                                sftp,
                                &path.to_string_lossy(),
                                depth + 1,
                                max_depth,
                                recursive,
                                out,
                            )?;
                        }
                    }
                    Ok(())
                }
                walk(sftp, &remote_path_clone, 0, max_depth, recursive, &mut out)?;
                Ok(out)
            })
            .await?;

        Ok(serde_json::json!({"success": true, "path": remote_path, "entries": entries}))
    }

    async fn sftp_exists(&self, args: &Value) -> Result<Value, ToolError> {
        let remote_path = self.validation.ensure_string(
            args.get("remote_path")
                .or_else(|| args.get("path"))
                .unwrap_or(&Value::Null),
            "remote_path",
            true,
        )?;
        let timeout_ms = std::cmp::min(
            read_positive_int(args.get("timeout_ms")).unwrap_or(10_000),
            resolve_tool_call_budget_ms(),
        );
        let remote_path_clone = remote_path.clone();
        let exists = self
            .with_sftp(args, move |sftp| {
                let _ = timeout_ms; // placeholder, sftp stat is blocking
                match sftp.stat(Path::new(&remote_path_clone)) {
                    Ok(stat) => Ok((true, Some(stat))),
                    Err(err) => {
                        let io_err: std::io::Error = err.into();
                        if io_err.kind() == std::io::ErrorKind::NotFound {
                            Ok((false, None))
                        } else {
                            Err(ToolError::internal(io_err.to_string()))
                        }
                    }
                }
            })
            .await?;
        Ok(serde_json::json!({
            "success": true,
            "remote_path": remote_path,
            "exists": exists.0,
            "stat": exists.1.map(|stat| serde_json::json!({
                "size": stat.size,
                "mode": stat.perm,
                "uid": stat.uid,
                "gid": stat.gid,
                "atime": stat.atime,
                "mtime": stat.mtime,
            })),
        }))
    }

    async fn sftp_upload(&self, args: &Value) -> Result<Value, ToolError> {
        let local_path = expand_home_path(self.validation.ensure_string(
            args.get("local_path").unwrap_or(&Value::Null),
            "local_path",
            true,
        )?);
        let remote_path = self.validation.ensure_string(
            args.get("remote_path").unwrap_or(&Value::Null),
            "remote_path",
            true,
        )?;
        let overwrite = args
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mkdirs = args
            .get("mkdirs")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let preserve_mtime = args
            .get("preserve_mtime")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let local_clone = local_path.clone();
        let remote_clone = remote_path.clone();

        self.with_sftp(args, move |sftp| {
            if !overwrite && sftp.stat(Path::new(&remote_clone)).is_ok() {
                return Err(ToolError::conflict(format!(
                    "Remote path already exists: {}",
                    remote_clone
                ))
                .with_hint("Set overwrite=true to replace it."));
            }
            if mkdirs {
                ensure_remote_dir(sftp, &remote_clone)?;
            }
            let mut local_file = fs::File::open(&local_clone).map_err(|err| {
                ToolError::invalid_params(format!("local_path must be readable: {}", err))
            })?;
            let mut remote_file = sftp
                .open_mode(
                    Path::new(&remote_clone),
                    OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
                    0o600,
                    OpenType::File,
                )
                .map_err(map_ssh_error)?;
            std::io::copy(&mut local_file, &mut remote_file)
                .map_err(|err| ToolError::internal(err.to_string()))?;
            if preserve_mtime {
                if let Ok(metadata) = fs::metadata(&local_clone) {
                    let atime = metadata
                        .accessed()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs());
                    let mtime = metadata
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs());
                    let stat = FileStat {
                        size: None,
                        uid: None,
                        gid: None,
                        perm: None,
                        atime,
                        mtime,
                    };
                    let _ = sftp.setstat(Path::new(&remote_clone), stat);
                }
            }
            Ok(())
        })
        .await?;

        Ok(
            serde_json::json!({"success": true, "local_path": local_path.display().to_string(), "remote_path": remote_path}),
        )
    }

    async fn sftp_download(&self, args: &Value) -> Result<Value, ToolError> {
        let remote_path = self.validation.ensure_string(
            args.get("remote_path").unwrap_or(&Value::Null),
            "remote_path",
            true,
        )?;
        let local_path = expand_home_path(self.validation.ensure_string(
            args.get("local_path").unwrap_or(&Value::Null),
            "local_path",
            true,
        )?);
        let overwrite = args
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mkdirs = args
            .get("mkdirs")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let preserve_mtime = args
            .get("preserve_mtime")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !overwrite && local_path.exists() {
            return Err(ToolError::conflict(format!(
                "Local path already exists: {}",
                local_path.display()
            ))
            .with_hint("Set overwrite=true to replace it."));
        }
        if mkdirs {
            if let Some(parent) = local_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
        }
        let tmp_path = local_path.with_extension(format!("tmp-{}", rand::random::<u32>()));
        let tmp_clone = tmp_path.clone();
        let remote_clone = remote_path.clone();
        let local_clone = local_path.clone();

        let remote_times = self
            .with_sftp(args, move |sftp| {
                let mut remote_file = sftp.open(Path::new(&remote_clone)).map_err(map_ssh_error)?;
                let mut tmp_file = fs::File::create(&tmp_clone).map_err(|err| {
                    ToolError::internal(format!("Failed to create temp file: {}", err))
                })?;
                std::io::copy(&mut remote_file, &mut tmp_file)
                    .map_err(|err| ToolError::internal(err.to_string()))?;
                let stat = if preserve_mtime {
                    sftp.stat(Path::new(&remote_clone)).ok()
                } else {
                    None
                };
                Ok(stat)
            })
            .await?;

        fs::rename(&tmp_path, &local_clone)
            .map_err(|err| ToolError::internal(format!("Failed to finalize download: {}", err)))?;

        if preserve_mtime {
            if let Some(stat) = remote_times {
                if let (Some(atime), Some(mtime)) = (stat.atime, stat.mtime) {
                    let atime = filetime::FileTime::from_unix_time(atime as i64, 0);
                    let mtime = filetime::FileTime::from_unix_time(mtime as i64, 0);
                    let _ = filetime::set_file_times(&local_clone, atime, mtime);
                }
            }
        }

        Ok(
            serde_json::json!({"success": true, "remote_path": remote_path, "local_path": local_path.display().to_string()}),
        )
    }

    pub async fn with_sftp<F, T>(&self, args: &Value, handler: F) -> Result<T, ToolError>
    where
        F: FnOnce(&ssh2::Sftp) -> Result<T, ToolError> + Send + 'static,
        T: Send + 'static,
    {
        let resolved = self.resolve_connection(args).await?;
        let profile_name = resolved.profile_name.clone();
        let profile_service = self.profile_service.clone();
        tokio::task::spawn_blocking(move || {
            let (session, observed) = connect_session(&resolved.connection)?;
            if let Some(profile) = profile_name.as_deref() {
                maybe_persist_tofu(&profile_service, profile, &resolved.connection, observed)?;
            }
            let sftp = session.sftp().map_err(map_ssh_error)?;
            handler(&sftp)
        })
        .await
        .map_err(|_| ToolError::internal("SSH SFTP task failed"))?
    }

    async fn resolve_connection(&self, args: &Value) -> Result<ResolvedConnection, ToolError> {
        let inline_connection = args.get("connection").is_some();
        if inline_connection {
            let connection = args
                .get("connection")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            let resolved = if let Some(resolver) = &self.secret_ref_resolver {
                resolver.resolve_deep(&connection, args).await?
            } else {
                connection.clone()
            };
            let connection = self.build_connection_from_value(&resolved, args)?;
            return Ok(ResolvedConnection {
                connection,
                profile_name: None,
            });
        }

        let profile_name = self.resolve_profile_name(args).await?;
        let profile_name = profile_name.ok_or_else(|| {
            ToolError::invalid_params("SSH connection requires profile_name or connection")
                .with_hint("Pass args.profile_name, or provide args.connection.")
        })?;

        let profile = self
            .profile_service
            .get_profile(&profile_name, Some(SSH_PROFILE_TYPE))?;
        let mut merged = profile
            .get("data")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        if let Some(secrets) = profile.get("secrets") {
            if let Some(obj) = secrets.as_object() {
                if let Value::Object(map) = &mut merged {
                    for (key, value) in obj {
                        map.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        let resolved = if let Some(resolver) = &self.secret_ref_resolver {
            resolver.resolve_deep(&merged, args).await?
        } else {
            merged.clone()
        };
        let connection = self.build_connection_from_value(&resolved, args)?;
        Ok(ResolvedConnection {
            connection,
            profile_name: Some(profile_name),
        })
    }

    async fn resolve_profile_name(&self, args: &Value) -> Result<Option<String>, ToolError> {
        if let Some(name) = args.get("profile_name").and_then(|v| v.as_str()) {
            return Ok(Some(
                self.validation.ensure_identifier(name, "profile_name")?,
            ));
        }
        if let Some(resolver) = &self.project_resolver {
            if let Ok(ctx) = resolver.resolve_context(args).await {
                if let Some(target) = ctx.as_ref().and_then(|v| v.get("target")) {
                    if let Some(name) = target.get("ssh_profile").and_then(|v| v.as_str()) {
                        return Ok(Some(
                            self.validation.ensure_identifier(name, "ssh_profile")?,
                        ));
                    }
                }
            }
        }
        let profiles = self.profile_service.list_profiles(Some(SSH_PROFILE_TYPE))?;
        if let Some(arr) = profiles.as_array() {
            if arr.len() == 1 {
                if let Some(name) = arr[0].get("name").and_then(|v| v.as_str()) {
                    return Ok(Some(name.to_string()));
                }
            }
            if arr.is_empty() {
                return Ok(None);
            }
            return Err(ToolError::invalid_params("profile_name is required when multiple profiles exist")
                .with_details(serde_json::json!({"known_profiles": arr.iter().filter_map(|v| v.get("name")).collect::<Vec<_>>() })));
        }
        Ok(None)
    }

    fn build_connection_from_value(
        &self,
        value: &Value,
        args: &Value,
    ) -> Result<SshConnection, ToolError> {
        let obj = value
            .as_object()
            .ok_or_else(|| ToolError::invalid_params("connection must be an object"))?;
        let host = obj
            .get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if host.is_empty() {
            return Err(ToolError::invalid_params("connection.host is required"));
        }
        let username = obj
            .get("username")
            .or_else(|| obj.get("user"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if username.is_empty() {
            return Err(ToolError::invalid_params("connection.username is required"));
        }
        let port = self
            .validation
            .ensure_port(obj.get("port"), Some(network_constants::SSH_DEFAULT_PORT))?;

        let mut private_key = obj
            .get("private_key")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if private_key.is_none() {
            if let Some(path) = obj.get("private_key_path").and_then(|v| v.as_str()) {
                let path = expand_home_path(path);
                private_key = fs::read_to_string(path).ok();
            }
        }
        let password = obj
            .get("password")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let passphrase = obj
            .get("passphrase")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if private_key.is_none() && password.is_none() {
            return Err(ToolError::invalid_params("Provide password or private_key for SSH connection")
                .with_hint("Set connection.password, or connection.private_key/private_key_path (optionally passphrase)."));
        }

        let ready_timeout_ms = obj
            .get("ready_timeout")
            .or_else(|| obj.get("ready_timeout_ms"))
            .and_then(|v| v.as_u64())
            .unwrap_or(network_constants::TIMEOUT_SSH_READY_MS);
        let keepalive_interval_ms = obj
            .get("keepalive_interval")
            .or_else(|| obj.get("keepalive_interval_ms"))
            .and_then(|v| v.as_u64())
            .unwrap_or(network_constants::KEEPALIVE_INTERVAL_MS);

        let policy = normalize_host_key_policy(
            args.get("host_key_policy")
                .or_else(|| obj.get("host_key_policy")),
        )?;
        let fingerprint = normalize_fingerprint_sha256(
            args.get("host_key_fingerprint_sha256")
                .or_else(|| obj.get("host_key_fingerprint_sha256")),
        );
        let policy = policy.unwrap_or_else(|| {
            if fingerprint.is_some() {
                HostKeyPolicy::Pin
            } else {
                HostKeyPolicy::Accept
            }
        });
        if policy == HostKeyPolicy::Pin && fingerprint.is_none() {
            return Err(ToolError::invalid_params(
                "host_key_fingerprint_sha256 is required for host_key_policy=pin",
            )
            .with_hint(
                "Set host_key_policy=accept (insecure) or provide host_key_fingerprint_sha256.",
            ));
        }

        Ok(SshConnection {
            host,
            port,
            username,
            password,
            private_key,
            passphrase,
            ready_timeout_ms,
            keepalive_interval_ms,
            host_key_policy: policy,
            host_key_fingerprint: fingerprint,
        })
    }

    fn register_job(&self, job: Value) {
        let job_id = job
            .get("job_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if job_id.is_empty() {
            return;
        }
        if let Some(service) = &self.job_service {
            let payload = serde_json::json!({
                "job_id": job_id,
                "kind": "ssh_detached",
                "status": "running",
                "profile_name": job.get("profile_name").cloned().unwrap_or(Value::Null),
                "pid": job.get("pid").cloned().unwrap_or(Value::Null),
                "pid_path": job.get("pid_path").cloned().unwrap_or(Value::Null),
                "log_path": job.get("log_path").cloned().unwrap_or(Value::Null),
                "exit_path": job.get("exit_path").cloned().unwrap_or(Value::Null),
                "provider": {
                    "tool": "mcp_ssh_manager",
                    "profile_name": job.get("profile_name").cloned().unwrap_or(Value::Null),
                    "pid": job.get("pid").cloned().unwrap_or(Value::Null),
                    "pid_path": job.get("pid_path").cloned().unwrap_or(Value::Null),
                    "log_path": job.get("log_path").cloned().unwrap_or(Value::Null),
                    "exit_path": job.get("exit_path").cloned().unwrap_or(Value::Null),
                },
                "created_at": job.get("created_at").cloned().unwrap_or(Value::String(chrono::Utc::now().to_rfc3339())),
            });
            let _ = service.upsert(payload);
        } else {
            self.jobs.insert(job_id.clone(), job.clone());
            while self.jobs.len() > self.max_jobs {
                if let Some(oldest) = self.jobs.iter().next().map(|entry| entry.key().clone()) {
                    self.jobs.remove(&oldest);
                } else {
                    break;
                }
            }
        }
    }

    fn resolve_job_spec(&self, args: &Value, require_log: bool) -> Result<JobSpec, ToolError> {
        let job_id = args
            .get("job_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let mut record = None;
        if let Some(job_id) = job_id.as_ref() {
            if let Some(service) = &self.job_service {
                record = service.get(job_id);
            } else if let Some(entry) = self.jobs.get(job_id) {
                record = Some(entry.value().clone());
            }
        }
        let not_found = job_id.is_some() && record.is_none();
        let pid = args.get("pid").and_then(|v| v.as_i64()).or_else(|| {
            record
                .as_ref()
                .and_then(|r| r.get("pid").and_then(|v| v.as_i64()))
        });
        let pid_path = args
            .get("pid_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                record.as_ref().and_then(|r| {
                    r.get("pid_path")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
            });
        let log_path = args
            .get("log_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                record.as_ref().and_then(|r| {
                    r.get("log_path")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
            });
        let exit_path = args
            .get("exit_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                record.as_ref().and_then(|r| {
                    r.get("exit_path")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
            });
        let profile_name = args
            .get("profile_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                record.as_ref().and_then(|r| {
                    r.get("profile_name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
            });

        if !not_found && pid.is_none() && pid_path.is_none() {
            return Err(ToolError::invalid_params("job requires pid or pid_path")
                .with_hint("Example: { action: 'job_status', pid_path: '/tmp/my.pid' }"));
        }
        if require_log && log_path.is_none() {
            return Err(ToolError::invalid_params("log_path is required")
                .with_hint("Example: { action: 'job_logs_tail', log_path: '/tmp/app.log' }"));
        }
        Ok(JobSpec {
            job_id: job_id.unwrap_or_default(),
            not_found,
            profile_name,
            pid,
            pid_path,
            log_path,
            exit_path,
        })
    }
}

#[derive(Debug, Clone)]
struct JobSpec {
    job_id: String,
    not_found: bool,
    profile_name: Option<String>,
    pid: Option<i64>,
    pid_path: Option<String>,
    log_path: Option<String>,
    exit_path: Option<String>,
}

#[derive(Debug, Clone)]
struct ExecResult {
    stdout: String,
    stderr: String,
    stdout_bytes: u64,
    stderr_bytes: u64,
    stdout_captured_bytes: usize,
    stderr_captured_bytes: usize,
    stdout_truncated: bool,
    stderr_truncated: bool,
    stdout_inline_truncated: bool,
    stderr_inline_truncated: bool,
    stdout_ref: Value,
    stderr_ref: Value,
    exit_code: i64,
    signal: Option<String>,
    timed_out: bool,
    hard_timed_out: bool,
    duration_ms: u128,
}

struct CaptureState {
    total: u64,
    captured: usize,
    truncated: bool,
    inline_truncated: bool,
    buffer: Vec<u8>,
    inline: Vec<u8>,
    writer: Option<ArtifactStream>,
    writer_limit: usize,
    writer_total: u64,
    writer_truncated: bool,
    max_capture: usize,
    max_inline: usize,
    filename: String,
    trace_id: Option<String>,
    span_id: Option<String>,
}

struct ArtifactStream {
    rel: String,
    uri: String,
    path: PathBuf,
    tmp_path: PathBuf,
    file: fs::File,
    bytes: u64,
}

impl ArtifactStream {
    fn new(
        filename: &str,
        trace_id: Option<&str>,
        span_id: Option<&str>,
    ) -> Result<Self, ToolError> {
        let context_root = resolve_context_root()
            .ok_or_else(|| ToolError::internal("Context root not available"))?;
        let reference = build_tool_call_file_ref(trace_id, span_id, filename)?;
        let path = crate::utils::artifacts::resolve_artifact_path(&context_root, &reference.rel)?;
        ensure_dir_for_file(&path).map_err(|err| {
            ToolError::internal(format!("Failed to create artifact dir: {}", err))
        })?;
        let tmp_path = temp_sibling_path(&path);
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(|err| ToolError::internal(format!("Failed to create artifact: {}", err)))?;
        Ok(Self {
            rel: reference.rel,
            uri: reference.uri,
            path,
            tmp_path,
            file,
            bytes: 0,
        })
    }

    fn write(&mut self, chunk: &[u8]) -> Result<(), ToolError> {
        self.file
            .write_all(chunk)
            .map_err(|err| ToolError::internal(format!("Failed to write artifact: {}", err)))?;
        self.bytes += chunk.len() as u64;
        Ok(())
    }

    fn finalize(mut self) -> Result<Value, ToolError> {
        self.file
            .flush()
            .map_err(|err| ToolError::internal(format!("Failed to flush artifact: {}", err)))?;
        drop(self.file);
        fs::rename(&self.tmp_path, &self.path)
            .map_err(|err| ToolError::internal(format!("Failed to finalize artifact: {}", err)))?;
        Ok(serde_json::json!({
            "uri": self.uri,
            "rel": self.rel,
            "bytes": self.bytes,
        }))
    }

    fn abort(self) {
        let _ = fs::remove_file(&self.tmp_path);
    }
}

fn resolve_tool_call_budget_ms() -> u64 {
    std::env::var("INFRA_TOOL_CALL_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(network_constants::TIMEOUT_MCP_TOOL_CALL_MS)
}

fn resolve_exec_default_timeout_ms() -> u64 {
    std::env::var("INFRA_SSH_EXEC_DEFAULT_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(network_constants::TIMEOUT_SSH_EXEC_DEFAULT_MS)
}

fn resolve_detached_start_timeout_ms() -> u64 {
    std::env::var("INFRA_SSH_DETACHED_START_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(network_constants::TIMEOUT_SSH_DETACHED_START_MS)
}

fn resolve_exec_max_capture_bytes() -> usize {
    std::env::var("INFRA_SSH_MAX_CAPTURE_BYTES")
        .or_else(|_| std::env::var("INFRA_MAX_CAPTURE_BYTES"))
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_CAPTURE_BYTES)
}

fn resolve_exec_max_inline_bytes() -> usize {
    std::env::var("INFRA_SSH_MAX_INLINE_BYTES")
        .or_else(|_| std::env::var("INFRA_MAX_INLINE_BYTES"))
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_INLINE_BYTES)
}

fn resolve_stream_to_artifact_mode() -> Option<String> {
    let raw = std::env::var("INFRA_SSH_STREAM_TO_ARTIFACT")
        .or_else(|_| std::env::var("INFRA_STREAM_TO_ARTIFACT"))
        .ok()?;
    let normalized = raw.trim().to_lowercase();
    if normalized.is_empty() {
        return None;
    }
    if normalized == "full" {
        return Some("full".to_string());
    }
    if normalized == "capped" {
        return Some("capped".to_string());
    }
    if normalized == "1" || normalized == "true" || normalized == "yes" {
        return Some("capped".to_string());
    }
    None
}

fn read_positive_int(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    if let Some(n) = value.as_i64() {
        if n > 0 {
            return Some(n as u64);
        }
    }
    if let Some(text) = value.as_str() {
        if let Ok(parsed) = text.parse::<u64>() {
            if parsed > 0 {
                return Some(parsed);
            }
        }
    }
    None
}

fn normalize_host_key_policy(value: Option<&Value>) -> Result<Option<HostKeyPolicy>, ToolError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let normalized = value
        .as_str()
        .unwrap_or(&value.to_string())
        .trim()
        .to_lowercase();
    if normalized.is_empty() {
        return Ok(None);
    }
    match normalized.as_str() {
        "accept" => Ok(Some(HostKeyPolicy::Accept)),
        "tofu" => Ok(Some(HostKeyPolicy::Tofu)),
        "pin" => Ok(Some(HostKeyPolicy::Pin)),
        _ => Err(
            ToolError::invalid_params(format!("Unknown host_key_policy: {}", normalized))
                .with_hint("Use one of: accept, tofu, pin."),
        ),
    }
}

fn normalize_fingerprint_sha256(value: Option<&Value>) -> Option<String> {
    let value = value.and_then(|v| v.as_str()).unwrap_or("");
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let cleaned = trimmed.trim_end_matches('=');
    if cleaned.to_lowercase().starts_with("sha256:") {
        return Some(format!("SHA256:{}", cleaned[7..].trim()));
    }
    Some(format!("SHA256:{}", cleaned))
}

fn fingerprint_host_key_sha256(session: &Session) -> Option<String> {
    let hash = session.host_key_hash(ssh2::HashType::Sha256)?;
    let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(hash);
    Some(format!("SHA256:{}", encoded))
}

fn escape_shell_value(value: &str) -> String {
    let escaped = value.replace('"', "\\\"");
    format!("'{}'", escaped.replace('\'', "'\\\''"))
}

fn resolve_public_key_line(args: &Value) -> Result<String, ToolError> {
    if let Some(key) = args.get("public_key").and_then(|v| v.as_str()) {
        return normalize_public_key_line(key);
    }
    if let Some(path) = args.get("public_key_path").and_then(|v| v.as_str()) {
        let raw = fs::read_to_string(expand_home_path(path)).map_err(|err| {
            ToolError::invalid_params(format!("public_key_path must be readable: {}", err))
        })?;
        return normalize_public_key_line(&raw);
    }
    Err(
        ToolError::invalid_params("public_key or public_key_path is required").with_hint(
            "Example: { action: 'authorized_keys_add', public_key: 'ssh-ed25519 AAAA... comment' }",
        ),
    )
}

fn normalize_public_key_line(raw: &str) -> Result<String, ToolError> {
    let normalized = raw.replace('\r', "");
    let lines: Vec<&str> = normalized
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    if lines.is_empty() {
        return Err(ToolError::invalid_params(
            "public_key must contain a single key line",
        ));
    }
    if lines.len() > 1 {
        return Err(
            ToolError::invalid_params("public_key must be a single key line")
                .with_hint("Remove extra lines/comments; keep exactly one key line."),
        );
    }
    let line = lines[0];
    if line.contains('\0') {
        return Err(ToolError::invalid_params(
            "public_key must not contain null bytes",
        ));
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 2 {
        return Err(ToolError::invalid_params("public_key has invalid format")
            .with_hint("Expected: \"<type> <base64> [comment]\"."));
    }
    Ok(line.to_string())
}

fn parse_public_key_tokens(line: &str) -> Result<(String, String), ToolError> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 2 {
        return Err(ToolError::invalid_params("public_key has invalid format")
            .with_hint("Expected: \"<type> <base64> [comment]\"."));
    }
    Ok((tokens[0].to_string(), tokens[1].to_string()))
}

fn fingerprint_public_key_sha256(line: &str) -> Result<String, ToolError> {
    let (_, key_blob) = parse_public_key_tokens(line)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(key_blob.as_bytes())
        .unwrap_or_else(|_| key_blob.as_bytes().to_vec());
    let hash = Sha256::digest(&bytes);
    let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(hash);
    Ok(format!("SHA256:{}", encoded))
}

fn build_command(
    security: &Security,
    command: &str,
    cwd: Option<&str>,
) -> Result<String, ToolError> {
    let trimmed = security.clean_command(command)?;
    if let Some(cwd) = cwd {
        return Ok(format!("cd {} && {}", escape_shell_value(cwd), trimmed));
    }
    Ok(trimmed)
}

fn collect_secret_values(env: &Option<Value>) -> Option<Vec<String>> {
    let env = env.as_ref()?;
    let obj = env.as_object()?;
    let mut out = Vec::new();
    for raw in obj.values() {
        if let Some(text) = raw.as_str() {
            let trimmed = text.trim();
            if trimmed.len() >= 6 {
                out.push(trimmed.to_string());
            }
        }
        if out.len() >= 32 {
            break;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn connect_session(connection: &SshConnection) -> Result<(Session, Option<String>), ToolError> {
    let addr = format!("{}:{}", connection.host, connection.port);
    let tcp = TcpStream::connect_timeout(
        &addr
            .parse()
            .map_err(|_| ToolError::invalid_params("Invalid SSH host/port"))?,
        Duration::from_millis(connection.ready_timeout_ms),
    )
    .map_err(|err| ToolError::internal(format!("Failed to connect SSH: {}", err)))?;
    tcp.set_read_timeout(Some(Duration::from_millis(connection.ready_timeout_ms)))
        .ok();
    tcp.set_write_timeout(Some(Duration::from_millis(connection.ready_timeout_ms)))
        .ok();

    let mut session =
        Session::new().map_err(|_| ToolError::internal("Failed to create SSH session"))?;
    session.set_tcp_stream(tcp);
    session.handshake().map_err(map_ssh_error)?;

    let observed = fingerprint_host_key_sha256(&session);
    if let Some(expected) = connection.host_key_fingerprint.as_ref() {
        if observed.as_ref() != Some(expected) {
            return Err(ToolError::denied(format!(
                "SSH host key mismatch (expected {}, got {})",
                expected,
                observed.clone().unwrap_or_else(|| "unknown".to_string())
            )));
        }
    } else if connection.host_key_policy == HostKeyPolicy::Pin {
        return Err(ToolError::invalid_params(
            "host_key_fingerprint_sha256 is required for host_key_policy=pin",
        ));
    }

    if let Some(key) = connection.private_key.as_ref() {
        session
            .userauth_pubkey_memory(
                &connection.username,
                None,
                key,
                connection.passphrase.as_deref(),
            )
            .map_err(map_ssh_error)?;
    } else if let Some(password) = connection.password.as_ref() {
        session
            .userauth_password(&connection.username, password)
            .map_err(map_ssh_error)?;
    }

    if !session.authenticated() {
        return Err(ToolError::denied("SSH authentication failed"));
    }
    let interval = std::cmp::max(1, (connection.keepalive_interval_ms / 1000) as u32);
    session.set_keepalive(true, interval);

    Ok((session, observed))
}

fn maybe_persist_tofu(
    profile_service: &ProfileService,
    profile_name: &str,
    connection: &SshConnection,
    observed: Option<String>,
) -> Result<(), ToolError> {
    if connection.host_key_policy != HostKeyPolicy::Tofu {
        return Ok(());
    }
    if connection.host_key_fingerprint.is_some() {
        return Ok(());
    }
    let Some(observed) = observed else {
        return Ok(());
    };
    let payload = serde_json::json!({
        "type": SSH_PROFILE_TYPE,
        "data": {
            "host_key_policy": "tofu",
            "host_key_fingerprint_sha256": observed,
        },
    });
    let _ = profile_service.set_profile(profile_name, &payload);
    Ok(())
}

fn test_connection(connection: &SshConnection) -> Result<(), ToolError> {
    let (_session, _observed) = connect_session(connection)?;
    Ok(())
}

fn exec_blocking(
    resolved: &ResolvedConnection,
    profile_service: Arc<ProfileService>,
    command: &str,
    env: Option<Value>,
    pty: bool,
    stdin: Option<StdinSource>,
    stdin_eof: bool,
    timeout_ms: Option<u64>,
    trace_id: Option<String>,
    span_id: Option<String>,
) -> Result<ExecResult, ToolError> {
    let (session, observed) = connect_session(&resolved.connection)?;
    if let Some(profile) = resolved.profile_name.as_deref() {
        let _ = maybe_persist_tofu(
            &profile_service,
            profile,
            &resolved.connection,
            observed.clone(),
        );
    }

    let mut channel = session.channel_session().map_err(map_ssh_error)?;
    if pty {
        let _ = channel.request_pty("xterm", None, None);
    }
    if let Some(env_map) = env.as_ref().and_then(|v| v.as_object()) {
        for (key, value) in env_map {
            if let Some(text) = value.as_str() {
                let _ = channel.setenv(key, text);
            } else {
                let _ = channel.setenv(key, &value.to_string());
            }
        }
    }
    channel.exec(command).map_err(map_ssh_error)?;
    session.set_blocking(false);

    let mut stdin_bytes: Option<Vec<u8>> = None;
    let mut stdin_file: Option<std::fs::File> = None;
    let mut stdin_done = false;
    match stdin {
        Some(StdinSource::Bytes(bytes)) => {
            if bytes.is_empty() {
                if stdin_eof {
                    let _ = channel.send_eof();
                }
                stdin_done = true;
            } else {
                stdin_bytes = Some(bytes);
            }
        }
        Some(StdinSource::File(path)) => {
            let file = std::fs::File::open(&path).map_err(|err| {
                ToolError::invalid_params(format!("stdin_file must be readable: {}", err))
            })?;
            stdin_file = Some(file);
        }
        None => {
            stdin_done = true;
        }
    }
    let mut stdin_offset = 0usize;
    let mut stdin_chunk: Vec<u8> = Vec::new();

    let max_capture = resolve_exec_max_capture_bytes();
    let max_inline = resolve_exec_max_inline_bytes();
    let stream_mode = resolve_stream_to_artifact_mode();
    let mut stdout_state = CaptureState::new(
        max_capture,
        max_inline,
        stream_mode.as_deref(),
        "stdout.log",
        trace_id.as_deref(),
        span_id.as_deref(),
    )?;
    let mut stderr_state = CaptureState::new(
        max_capture,
        max_inline,
        stream_mode.as_deref(),
        "stderr.log",
        trace_id.as_deref(),
        span_id.as_deref(),
    )?;

    let mut stderr_stream = channel.stderr();
    let started = Instant::now();
    let mut timed_out = false;
    let mut hard_timed_out = false;

    loop {
        let mut progressed = false;
        let mut buf = [0u8; 8192];
        if !stdin_done {
            if let Some(bytes) = stdin_bytes.as_ref() {
                match channel.write(&bytes[stdin_offset..]) {
                    Ok(n) if n > 0 => {
                        stdin_offset = std::cmp::min(stdin_offset + n, bytes.len());
                        progressed = true;
                        if stdin_offset >= bytes.len() {
                            if stdin_eof {
                                let _ = channel.send_eof();
                            }
                            stdin_done = true;
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        let io_err = err;
                        if io_err.kind() != std::io::ErrorKind::WouldBlock {
                            stdin_done = true;
                        }
                    }
                }
            } else if let Some(file) = stdin_file.as_mut() {
                if stdin_offset >= stdin_chunk.len() {
                    stdin_chunk.clear();
                    stdin_offset = 0;
                    let mut buf = [0u8; 8192];
                    let n = match file.read(&mut buf) {
                        Ok(n) => n,
                        Err(err) => {
                            return Err(ToolError::internal(format!(
                                "Failed to read stdin_file: {}",
                                err
                            )));
                        }
                    };
                    if n == 0 {
                        if stdin_eof {
                            let _ = channel.send_eof();
                        }
                        stdin_done = true;
                    } else {
                        stdin_chunk.extend_from_slice(&buf[..n]);
                    }
                }
                if !stdin_done {
                    match channel.write(&stdin_chunk[stdin_offset..]) {
                        Ok(n) if n > 0 => {
                            stdin_offset = std::cmp::min(stdin_offset + n, stdin_chunk.len());
                            progressed = true;
                        }
                        Ok(_) => {}
                        Err(err) => {
                            let io_err = err;
                            if io_err.kind() != std::io::ErrorKind::WouldBlock {
                                stdin_done = true;
                            }
                        }
                    }
                }
            } else {
                stdin_done = true;
            }
        }
        match channel.read(&mut buf) {
            Ok(n) if n > 0 => {
                stdout_state.capture(&buf[..n]);
                progressed = true;
            }
            Ok(_) => {}
            Err(err) => {
                let io_err = err;
                if io_err.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(ToolError::internal(format!(
                        "SSH stdout read failed: {}",
                        io_err
                    )));
                }
            }
        }
        match stderr_stream.read(&mut buf) {
            Ok(n) if n > 0 => {
                stderr_state.capture(&buf[..n]);
                progressed = true;
            }
            Ok(_) => {}
            Err(err) => {
                let io_err = err;
                if io_err.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(ToolError::internal(format!(
                        "SSH stderr read failed: {}",
                        io_err
                    )));
                }
            }
        }

        if channel.eof() {
            break;
        }
        if let Some(timeout) = timeout_ms {
            if started.elapsed().as_millis() as u64 > timeout {
                timed_out = true;
                break;
            }
        }
        if !progressed {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    if timed_out {
        let _ = channel.close();
        let grace = network_constants::TIMEOUT_SSH_EXEC_HARD_GRACE_MS;
        let deadline = Instant::now() + Duration::from_millis(grace);
        while Instant::now() < deadline {
            if channel.eof() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !channel.eof() {
            hard_timed_out = true;
        }
    }

    let _ = channel.wait_close();
    let exit_code = i64::from(channel.exit_status().unwrap_or(-1));
    let signal = channel.exit_signal().ok().and_then(|sig| sig.exit_signal);

    let extra_secrets = collect_secret_values(&env);
    let stdout = redact_text(
        &stdout_state.inline_string(),
        usize::MAX,
        extra_secrets.as_deref(),
    );
    let stderr = redact_text(
        &stderr_state.inline_string(),
        usize::MAX,
        extra_secrets.as_deref(),
    );

    let stdout_ref = stdout_state.finalize_artifact(extra_secrets.as_deref())?;
    let stderr_ref = stderr_state.finalize_artifact(extra_secrets.as_deref())?;

    Ok(ExecResult {
        stdout,
        stderr,
        stdout_bytes: stdout_state.total,
        stderr_bytes: stderr_state.total,
        stdout_captured_bytes: stdout_state.captured,
        stderr_captured_bytes: stderr_state.captured,
        stdout_truncated: stdout_state.truncated,
        stderr_truncated: stderr_state.truncated,
        stdout_inline_truncated: stdout_state.inline_truncated,
        stderr_inline_truncated: stderr_state.inline_truncated,
        stdout_ref,
        stderr_ref,
        exit_code,
        signal,
        timed_out,
        hard_timed_out,
        duration_ms: started.elapsed().as_millis(),
    })
}

impl CaptureState {
    fn new(
        max_capture: usize,
        max_inline: usize,
        stream_mode: Option<&str>,
        filename: &str,
        trace_id: Option<&str>,
        span_id: Option<&str>,
    ) -> Result<Self, ToolError> {
        let writer = if stream_mode.is_some() {
            ArtifactStream::new(filename, trace_id, span_id).ok()
        } else {
            None
        };
        let writer_limit = if stream_mode == Some("full") {
            usize::MAX
        } else {
            max_capture
        };
        Ok(Self {
            total: 0,
            captured: 0,
            truncated: false,
            inline_truncated: false,
            buffer: Vec::new(),
            inline: Vec::new(),
            writer,
            writer_limit,
            writer_total: 0,
            writer_truncated: false,
            max_capture,
            max_inline,
            filename: filename.to_string(),
            trace_id: trace_id.map(|s| s.to_string()),
            span_id: span_id.map(|s| s.to_string()),
        })
    }

    fn capture(&mut self, chunk: &[u8]) {
        self.total += chunk.len() as u64;
        if let Some(writer) = self.writer.as_mut() {
            if self.writer_total < self.writer_limit as u64 {
                let remaining =
                    (self.writer_limit as u64).saturating_sub(self.writer_total) as usize;
                let slice = if chunk.len() > remaining {
                    &chunk[..remaining]
                } else {
                    chunk
                };
                let _ = writer.write(slice);
                self.writer_total += slice.len() as u64;
                if slice.len() < chunk.len() {
                    self.writer_truncated = true;
                }
            } else {
                self.writer_truncated = true;
            }
        }
        if self.captured < self.max_capture {
            let remaining = self.max_capture - self.captured;
            let slice = if chunk.len() > remaining {
                &chunk[..remaining]
            } else {
                chunk
            };
            self.buffer.extend_from_slice(slice);
            self.captured += slice.len();
            if slice.len() < chunk.len() {
                self.truncated = true;
            }
        } else {
            self.truncated = true;
        }
        if self.inline.len() < self.max_inline {
            let remaining = self.max_inline - self.inline.len();
            let slice = if chunk.len() > remaining {
                &chunk[..remaining]
            } else {
                chunk
            };
            self.inline.extend_from_slice(slice);
            if slice.len() < chunk.len() {
                self.inline_truncated = true;
            }
        } else {
            self.inline_truncated = true;
        }
    }

    fn inline_string(&self) -> String {
        String::from_utf8_lossy(&self.inline).to_string()
    }

    fn finalize_artifact(&mut self, extra_secrets: Option<&[String]>) -> Result<Value, ToolError> {
        if let Some(writer) = self.writer.take() {
            if self.writer_total == 0 {
                writer.abort();
                return Ok(Value::Null);
            }
            let mut payload = writer.finalize()?;
            if let Value::Object(map) = &mut payload {
                map.insert(
                    "captured_bytes".to_string(),
                    Value::Number(self.writer_total.into()),
                );
                map.insert("total_bytes".to_string(), Value::Number(self.total.into()));
                map.insert("truncated".to_string(), Value::Bool(self.writer_truncated));
            }
            return Ok(payload);
        }

        if self.buffer.is_empty() {
            return Ok(Value::Null);
        }
        let context_root = resolve_context_root();
        if context_root.is_none() {
            return Ok(Value::Null);
        }
        if !(self.truncated || self.inline_truncated) {
            return Ok(Value::Null);
        }
        if let Ok(reference) = build_tool_call_file_ref(
            self.trace_id.as_deref(),
            self.span_id.as_deref(),
            &self.filename,
        ) {
            let redacted = redact_text(
                &String::from_utf8_lossy(&self.buffer),
                usize::MAX,
                extra_secrets,
            );
            let written =
                write_text_artifact(context_root.as_ref().unwrap(), &reference, &redacted)?;
            return Ok(serde_json::json!({
                "uri": written.uri,
                "rel": written.rel,
                "bytes": written.bytes,
            }));
        }
        Ok(Value::Null)
    }
}

fn map_ssh_error(err: ssh2::Error) -> ToolError {
    let io_err: std::io::Error = err.into();
    match io_err.kind() {
        std::io::ErrorKind::TimedOut => ToolError::timeout("SSH operation timed out"),
        std::io::ErrorKind::WouldBlock => ToolError::retryable("SSH operation would block"),
        _ => ToolError::internal(format!("SSH error: {}", io_err)),
    }
}

pub(crate) fn ensure_remote_dir(sftp: &ssh2::Sftp, remote_path: &str) -> Result<(), ToolError> {
    let path = Path::new(remote_path);
    let mut current = PathBuf::new();
    if let Some(parent) = path.parent() {
        for part in parent.components() {
            current.push(part);
            if current.as_os_str().is_empty() {
                continue;
            }
            if sftp.stat(&current).is_ok() {
                continue;
            }
            let _ = sftp.mkdir(&current, 0o755);
        }
    }
    Ok(())
}

fn compute_local_sha256_hex(path: &Path) -> Result<String, ToolError> {
    let mut file = fs::File::open(path).map_err(|err| {
        ToolError::invalid_params(format!("local_path must be readable: {}", err))
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|err| ToolError::internal(err.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn build_remote_sha256_command(remote_path: &str) -> String {
    let quoted = escape_shell_value(remote_path);
    [
        "set -u".to_string(),
        format!("PATH_ARG={}", quoted),
        "if command -v sha256sum >/dev/null 2>&1; then sha256sum -- \"$PATH_ARG\" 2>/dev/null | awk '{print $1}'; exit 0; fi".to_string(),
        "if command -v shasum >/dev/null 2>&1; then shasum -a 256 -- \"$PATH_ARG\" 2>/dev/null | awk '{print $1}'; exit 0; fi".to_string(),
        "if command -v openssl >/dev/null 2>&1; then openssl dgst -sha256 -- \"$PATH_ARG\" 2>/dev/null | awk '{print $NF}'; exit 0; fi".to_string(),
        "echo \"__INFRA_NO_SHA256__\"".to_string(),
        "exit 127".to_string(),
    ]
    .join("\n")
}

fn parse_sha256_from_output(text: &str) -> Option<String> {
    let re = Regex::new(r"\b[a-fA-F0-9]{64}\b").ok()?;
    let caps = re.find(text)?;
    Some(caps.as_str().to_lowercase())
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for SshManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
