use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::utils::sandbox::resolve_sandbox_path;
use crate::utils::template::resolve_templates;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::path::PathBuf;
use tokio::process::Command;

const REPO_ACTIONS: &[&str] = &[
    "exec",
    "repo_info",
    "assert_clean",
    "git_diff",
    "render",
    "apply_patch",
    "git_commit",
    "git_revert",
    "git_push",
];

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

fn split_allowlist(raw: Option<String>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .collect()
}

fn allowed_commands() -> Vec<String> {
    let raw = std::env::var("INFRA_REPO_ALLOWED_COMMANDS").ok();
    let list = split_allowlist(raw);
    if list.is_empty() {
        vec!["git".to_string()]
    } else {
        list
    }
}

#[derive(Clone)]
pub struct RepoManager {
    logger: Logger,
}

impl RepoManager {
    pub fn new(logger: Logger) -> Self {
        Self {
            logger: logger.child("repo"),
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "exec" => self.exec(args).await,
            "repo_info" => self.repo_info(args).await,
            "assert_clean" => self.assert_clean(args).await,
            "git_diff" => self.git_diff(args).await,
            "render" => self.render(args).await,
            "apply_patch" => self.apply_patch(args).await,
            "git_commit" => self.git_commit(args).await,
            "git_revert" => self.git_revert(args).await,
            "git_push" => self.git_push(args).await,
            _ => Err(unknown_action_error("repo", action, REPO_ACTIONS)),
        }
    }

    fn resolve_repo_root(&self, args: &Value) -> Result<PathBuf, ToolError> {
        let raw = args
            .get("repo_root")
            .or_else(|| args.get("repo_path"))
            .or_else(|| args.get("root"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::invalid_params("repo_root is required"))?;
        let path = PathBuf::from(raw);
        resolve_sandbox_path(&path, None, true)
    }

    async fn run_command(
        &self,
        cwd: &PathBuf,
        command: &str,
        args: &[String],
        timeout_ms: Option<u64>,
    ) -> Result<(i32, Vec<u8>, Vec<u8>), ToolError> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        cmd.current_dir(cwd);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let output = if let Some(timeout) = timeout_ms {
            match tokio::time::timeout(std::time::Duration::from_millis(timeout), cmd.output())
                .await
            {
                Ok(result) => {
                    result.map_err(|err| ToolError::internal(format!("exec failed: {}", err)))?
                }
                Err(_) => {
                    return Err(ToolError::timeout("Command timed out"));
                }
            }
        } else {
            cmd.output()
                .await
                .map_err(|err| ToolError::internal(format!("exec failed: {}", err)))?
        };

        let code = output.status.code().unwrap_or(-1);
        Ok((code, output.stdout, output.stderr))
    }

    async fn exec(&self, args: Value) -> Result<Value, ToolError> {
        let repo_root = self.resolve_repo_root(&args)?;
        let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
        if command.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "command must be a non-empty string",
            ));
        }
        let allowed = allowed_commands();
        if !allowed.iter().any(|c| c == command || c == "*") {
            return Err(
                ToolError::denied(format!("Command '{}' is not allowed", command)).with_hint(
                    "Set INFRA_REPO_ALLOWED_COMMANDS to allow additional commands.".to_string(),
                ),
            );
        }
        let argv: Vec<String> = args
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|v| v.as_str().unwrap_or(&v.to_string()).to_string())
                    .collect()
            })
            .unwrap_or_default();
        let timeout_ms = read_positive_int(args.get("timeout_ms"));
        let (code, stdout, stderr) = self
            .run_command(&repo_root, command, &argv, timeout_ms)
            .await?;
        let max_inline = 16 * 1024;
        let stdout_inline =
            String::from_utf8_lossy(&stdout[..stdout.len().min(max_inline)]).to_string();
        let stderr_inline =
            String::from_utf8_lossy(&stderr[..stderr.len().min(max_inline)]).to_string();
        Ok(serde_json::json!({
            "success": code == 0,
            "exit_code": code,
            "timed_out": false,
            "stdout_inline": stdout_inline,
            "stderr_inline": stderr_inline,
            "stdout_bytes": stdout.len(),
            "stderr_bytes": stderr.len(),
            "stdout_truncated": stdout.len() > max_inline,
            "stderr_truncated": stderr.len() > max_inline,
        }))
    }

    async fn repo_info(&self, args: Value) -> Result<Value, ToolError> {
        let repo_root = self.resolve_repo_root(&args)?;
        Ok(serde_json::json!({"success": true, "repo_root": repo_root}))
    }

    async fn assert_clean(&self, args: Value) -> Result<Value, ToolError> {
        let repo_root = self.resolve_repo_root(&args)?;
        let argv = ["status".to_string(), "--porcelain".to_string()];
        let (code, stdout, stderr) = self
            .run_command(&repo_root, "git", &argv, Some(30_000))
            .await?;
        if code != 0 {
            return Err(
                ToolError::internal("git status failed").with_details(serde_json::json!({
                    "stderr": String::from_utf8_lossy(&stderr),
                })),
            );
        }
        let output = String::from_utf8_lossy(&stdout).trim().to_string();
        if !output.is_empty() {
            return Err(ToolError::conflict(
                "Repository is dirty (uncommitted changes).",
            ));
        }
        Ok(serde_json::json!({"success": true, "repo_root": repo_root, "clean": true}))
    }

    async fn git_diff(&self, args: Value) -> Result<Value, ToolError> {
        let repo_root = self.resolve_repo_root(&args)?;
        let mut argv = vec!["diff".to_string()];
        if let Some(paths) = args.get("paths").and_then(|v| v.as_array()) {
            argv.push("--".to_string());
            for entry in paths.iter().filter_map(|v| v.as_str()) {
                argv.push(entry.to_string());
            }
        }
        let (code, stdout, stderr) = self
            .run_command(&repo_root, "git", &argv, Some(30_000))
            .await?;
        if code != 0 {
            return Err(
                ToolError::internal("git diff failed").with_details(serde_json::json!({
                    "stderr": String::from_utf8_lossy(&stderr),
                })),
            );
        }
        Ok(serde_json::json!({
            "success": true,
            "diff": String::from_utf8_lossy(&stdout).to_string(),
        }))
    }

    async fn render(&self, args: Value) -> Result<Value, ToolError> {
        let template = args.get("template").and_then(|v| v.as_str()).unwrap_or("");
        let context = args.get("context").cloned().unwrap_or_else(|| {
            args.get("data")
                .cloned()
                .unwrap_or(Value::Object(Default::default()))
        });
        let missing = args
            .get("template_missing")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        let rendered = resolve_templates(&Value::String(template.to_string()), &context, missing)?;
        Ok(serde_json::json!({"success": true, "rendered": rendered}))
    }

    async fn apply_patch(&self, args: Value) -> Result<Value, ToolError> {
        let repo_root = self.resolve_repo_root(&args)?;
        let apply = args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false);
        let patch = args
            .get("patch")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::invalid_params("patch is required"))?;
        let patch_path = repo_root.join(".infra.patch");
        tokio::fs::write(&patch_path, patch)
            .await
            .map_err(|err| ToolError::internal(err.to_string()))?;
        let mut argv = vec!["apply".to_string()];
        if !apply {
            argv.push("--check".to_string());
        }
        argv.push(patch_path.to_string_lossy().to_string());
        let (code, stdout, stderr) = self
            .run_command(&repo_root, "git", &argv, Some(30_000))
            .await?;
        let _ = tokio::fs::remove_file(&patch_path).await;
        Ok(serde_json::json!({
            "success": code == 0,
            "exit_code": code,
            "stdout": String::from_utf8_lossy(&stdout),
            "stderr": String::from_utf8_lossy(&stderr),
        }))
    }

    async fn git_commit(&self, args: Value) -> Result<Value, ToolError> {
        let repo_root = self.resolve_repo_root(&args)?;
        let apply = args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false);
        if !apply {
            return Err(ToolError::denied("git_commit requires apply=true"));
        }
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::invalid_params("message is required"))?;
        let argv = vec!["commit".to_string(), "-m".to_string(), message.to_string()];
        let (code, stdout, stderr) = self
            .run_command(&repo_root, "git", &argv, Some(30_000))
            .await?;
        Ok(
            serde_json::json!({"success": code == 0, "stdout": String::from_utf8_lossy(&stdout), "stderr": String::from_utf8_lossy(&stderr)}),
        )
    }

    async fn git_revert(&self, args: Value) -> Result<Value, ToolError> {
        let repo_root = self.resolve_repo_root(&args)?;
        let apply = args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false);
        if !apply {
            return Err(ToolError::denied("git_revert requires apply=true"));
        }
        let sha = args
            .get("sha")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::invalid_params("sha is required"))?;
        let argv = vec!["revert".to_string(), sha.to_string()];
        let (code, stdout, stderr) = self
            .run_command(&repo_root, "git", &argv, Some(30_000))
            .await?;
        Ok(
            serde_json::json!({"success": code == 0, "stdout": String::from_utf8_lossy(&stdout), "stderr": String::from_utf8_lossy(&stderr)}),
        )
    }

    async fn git_push(&self, args: Value) -> Result<Value, ToolError> {
        let repo_root = self.resolve_repo_root(&args)?;
        let apply = args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false);
        if !apply {
            return Err(ToolError::denied("git_push requires apply=true"));
        }
        let remote = args
            .get("remote")
            .and_then(|v| v.as_str())
            .unwrap_or("origin");
        let branch = args
            .get("branch")
            .and_then(|v| v.as_str())
            .unwrap_or("HEAD");
        let argv = vec!["push".to_string(), remote.to_string(), branch.to_string()];
        let (code, stdout, stderr) = self
            .run_command(&repo_root, "git", &argv, Some(60_000))
            .await?;
        Ok(
            serde_json::json!({"success": code == 0, "stdout": String::from_utf8_lossy(&stdout), "stderr": String::from_utf8_lossy(&stderr)}),
        )
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for RepoManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
