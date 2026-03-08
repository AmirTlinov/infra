use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::utils::artifacts::{
    build_tool_call_file_ref, resolve_context_root, write_text_artifact,
};
use crate::utils::sandbox::resolve_sandbox_path;
use crate::utils::stdin::{resolve_stdin_source, StdinSource};
use crate::utils::tool_errors::unknown_action_error;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

pub(crate) const REPO_ACTIONS: &[&str] = &[
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

static IMAGE_LINE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*image\s*:\s*(?P<image>.+?)\s*$").unwrap());

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn strip_yaml_scalar(value: &str) -> String {
    let mut trimmed = value.trim().to_string();
    if let Some((left, _)) = trimmed.split_once('#') {
        // Best-effort: drop trailing comments.
        trimmed = left.trim().to_string();
    }
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        trimmed = trimmed[1..trimmed.len() - 1].to_string();
    }
    trimmed.trim().to_string()
}

fn image_tag(image: &str) -> Option<String> {
    let before_digest = image.split('@').next().unwrap_or(image);
    let slash = before_digest.rfind('/').unwrap_or(0);
    let colon = before_digest.rfind(':');
    if let Some(idx) = colon {
        if idx > slash {
            return Some(before_digest[idx + 1..].to_string());
        }
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
        stdin: Option<StdinSource>,
        stdin_eof: bool,
    ) -> Result<(i32, Vec<u8>, Vec<u8>, bool), ToolError> {
        let allowed = allowed_commands();
        if !allowed.iter().any(|c| c == command || c == "*") {
            return Err(
                ToolError::denied(format!("Command '{}' is not allowed", command)).with_hint(
                    "Set INFRA_REPO_ALLOWED_COMMANDS to allow additional commands.".to_string(),
                ),
            );
        }

        let mut cmd = Command::new(command);
        cmd.args(args);
        cmd.current_dir(cwd);
        if stdin.is_some() {
            cmd.stdin(std::process::Stdio::piped());
        } else {
            cmd.stdin(std::process::Stdio::null());
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|err| ToolError::internal(format!("exec failed: {}", err)))?;
        let mut stdin_hold = None;
        if let Some(input) = stdin {
            if let Some(mut writer) = child.stdin.take() {
                match input {
                    StdinSource::Bytes(bytes) => {
                        if !bytes.is_empty() {
                            writer.write_all(&bytes).await.map_err(|err| {
                                ToolError::internal(format!("stdin write failed: {}", err))
                            })?;
                        }
                    }
                    StdinSource::File(path) => {
                        let mut file = tokio::fs::File::open(&path).await.map_err(|err| {
                            ToolError::invalid_params(format!(
                                "stdin_file must be readable: {}",
                                err
                            ))
                        })?;
                        tokio::io::copy(&mut file, &mut writer)
                            .await
                            .map_err(|err| {
                                ToolError::internal(format!("stdin file stream failed: {}", err))
                            })?;
                    }
                }
                if !stdin_eof {
                    stdin_hold = Some(writer);
                }
            }
        }

        let mut stdout_reader = child.stdout.take();
        let mut stderr_reader = child.stderr.take();
        let stdout_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut reader) = stdout_reader.take() {
                let _ = reader.read_to_end(&mut buf).await;
            }
            buf
        });
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut reader) = stderr_reader.take() {
                let _ = reader.read_to_end(&mut buf).await;
            }
            buf
        });

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
        }
        .map_err(|err| ToolError::internal(format!("exec failed: {}", err)))?;

        drop(stdin_hold);
        let stdout = stdout_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();
        let code = status.code().unwrap_or(-1);
        Ok((code, stdout, stderr, timed_out))
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
        let stdin = resolve_stdin_source(&args)?;
        let stdin_eof = args
            .get("stdin_eof")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let (code, stdout, stderr, timed_out) = self
            .run_command(&repo_root, command, &argv, timeout_ms, stdin, stdin_eof)
            .await?;
        let max_inline = 16 * 1024;
        let stdout_inline =
            String::from_utf8_lossy(&stdout[..stdout.len().min(max_inline)]).to_string();
        let stderr_inline =
            String::from_utf8_lossy(&stderr[..stderr.len().min(max_inline)]).to_string();
        Ok(serde_json::json!({
            "success": code == 0,
            "exit_code": code,
            "timed_out": timed_out,
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
        let (code, stdout, stderr, _) = self
            .run_command(&repo_root, "git", &argv, Some(30_000), None, true)
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
        let max_capture_bytes = read_positive_int(args.get("max_bytes")).unwrap_or(10_000_000);

        let mut argv = vec!["diff".to_string(), "--no-color".to_string()];
        if let Some(paths) = args.get("paths").and_then(|v| v.as_array()) {
            argv.push("--".to_string());
            for entry in paths.iter().filter_map(|v| v.as_str()) {
                argv.push(entry.to_string());
            }
        }

        let (code, stdout, stderr, _) = self
            .run_command(&repo_root, "git", &argv, Some(30_000), None, true)
            .await?;
        if code != 0 {
            return Err(
                ToolError::internal("git diff failed").with_details(serde_json::json!({
                    "stderr": String::from_utf8_lossy(&stderr),
                })),
            );
        }

        let truncated = stdout.len() as u64 > max_capture_bytes;
        let captured = if truncated {
            &stdout[..(max_capture_bytes as usize).min(stdout.len())]
        } else {
            stdout.as_slice()
        };

        let diff_sha256 = sha256_hex(captured);
        let mut diff_ref = Value::Null;
        if let Some(root) = resolve_context_root() {
            let trace_id = args.get("trace_id").and_then(|v| v.as_str());
            let span_id = args.get("span_id").and_then(|v| v.as_str());
            if let Ok(reference) = build_tool_call_file_ref(trace_id, span_id, "git-diff.patch") {
                let content = String::from_utf8_lossy(captured).to_string();
                let written = write_text_artifact(&root, &reference, &content)?;
                diff_ref = Value::String(written.uri);
            }
        }

        // Diffstat is useful for quick planning and is kept inline.
        let mut stat_argv = vec!["diff".to_string(), "--stat".to_string()];
        if let Some(paths) = args.get("paths").and_then(|v| v.as_array()) {
            stat_argv.push("--".to_string());
            for entry in paths.iter().filter_map(|v| v.as_str()) {
                stat_argv.push(entry.to_string());
            }
        }
        let diffstat = match self
            .run_command(&repo_root, "git", &stat_argv, Some(30_000), None, true)
            .await
        {
            Ok((0, out, _err, _)) => String::from_utf8_lossy(&out).to_string(),
            Ok((_code, _out, err, _)) => {
                format!("[diffstat unavailable: {}]", String::from_utf8_lossy(&err))
            }
            Err(err) => format!("[diffstat unavailable: {}]", err.message),
        };

        Ok(serde_json::json!({
            "success": true,
            "diff_ref": diff_ref,
            "diff_sha256": diff_sha256,
            "diff_truncated": truncated,
            "diff_bytes": stdout.len(),
            "diff_captured_bytes": captured.len(),
            "diffstat": diffstat,
        }))
    }

    async fn render(&self, args: Value) -> Result<Value, ToolError> {
        let repo_root = self.resolve_repo_root(&args)?;

        let requested_type = args
            .get("render_type")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_lowercase());
        let overlay = args
            .get("overlay")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let chart = args
            .get("chart")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let values: Vec<String> = if let Some(arr) = args.get("values").and_then(|v| v.as_array()) {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        } else if let Some(text) = args.get("values").and_then(|v| v.as_str()) {
            vec![text.to_string()]
        } else {
            Vec::new()
        };

        let inferred = if !chart.as_deref().unwrap_or("").trim().is_empty() {
            "helm".to_string()
        } else if !overlay.as_deref().unwrap_or("").trim().is_empty() {
            "kustomize".to_string()
        } else {
            "plain".to_string()
        };
        let render_type = requested_type.unwrap_or(inferred);

        let overlay_path = if let Some(overlay) = overlay.as_deref() {
            let candidate = std::path::PathBuf::from(overlay);
            Some(resolve_sandbox_path(&repo_root, Some(&candidate), true)?)
        } else {
            None
        };
        let chart_path = if let Some(chart) = chart.as_deref() {
            let candidate = std::path::PathBuf::from(chart);
            Some(resolve_sandbox_path(&repo_root, Some(&candidate), true)?)
        } else {
            None
        };
        let mut values_paths = Vec::new();
        for entry in values.iter() {
            let candidate = std::path::PathBuf::from(entry);
            values_paths.push(resolve_sandbox_path(&repo_root, Some(&candidate), true)?);
        }

        let max_capture_bytes =
            read_positive_int(args.get("max_bytes")).unwrap_or(25_000_000) as usize;

        let (command, argv): (&str, Vec<String>) = match render_type.as_str() {
            "plain" => ("", Vec::new()),
            "kustomize" => {
                let overlay = overlay_path.clone().unwrap_or_else(|| repo_root.clone());
                (
                    "kubectl",
                    vec![
                        "kustomize".to_string(),
                        overlay.to_string_lossy().to_string(),
                    ],
                )
            }
            "helm" => {
                let Some(chart_path) = chart_path.clone() else {
                    return Err(ToolError::invalid_params(
                        "chart is required for render_type=helm",
                    ));
                };
                let mut argv = vec![
                    "template".to_string(),
                    "infra".to_string(),
                    chart_path.to_string_lossy().to_string(),
                ];
                for path in values_paths.iter() {
                    argv.push("-f".to_string());
                    argv.push(path.to_string_lossy().to_string());
                }
                ("helm", argv)
            }
            other => {
                return Err(ToolError::invalid_params(format!(
                    "Invalid render_type: {} (expected plain|kustomize|helm)",
                    other
                )))
            }
        };

        let rendered_bytes: Vec<u8> = if render_type == "plain" {
            let root = overlay_path.clone().unwrap_or_else(|| repo_root.clone());
            if root.is_file() {
                tokio::fs::read(&root)
                    .await
                    .map_err(|err| ToolError::internal(format!("Failed to read file: {}", err)))?
            } else {
                let mut files = Vec::new();
                for entry in walkdir::WalkDir::new(&root)
                    .into_iter()
                    .filter_map(Result::ok)
                {
                    if !entry.file_type().is_file() {
                        continue;
                    }
                    let path = entry.path();
                    let ext = path
                        .extension()
                        .and_then(|v| v.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    if ext == "yml" || ext == "yaml" {
                        files.push(path.to_path_buf());
                    }
                    if files.len() >= 1000 {
                        break;
                    }
                }
                files.sort();
                let mut combined = String::new();
                for (idx, file) in files.iter().enumerate() {
                    let content = tokio::fs::read_to_string(file).await.map_err(|err| {
                        ToolError::internal(format!(
                            "Failed to read yaml file {}: {}",
                            file.display(),
                            err
                        ))
                    })?;
                    if idx > 0 {
                        combined.push_str("\n---\n");
                    }
                    combined.push_str(&content);
                }
                combined.into_bytes()
            }
        } else {
            let (code, stdout, stderr, timed_out) = self
                .run_command(&repo_root, command, &argv, Some(60_000), None, true)
                .await?;
            if timed_out {
                return Err(ToolError::timeout("render command timed out")
                    .with_details(serde_json::json!({"command": command, "args": argv})));
            }
            if code != 0 {
                // Best-effort: if kubectl is missing, hint about installing it.
                let stderr_text = String::from_utf8_lossy(&stderr).to_string();
                let hint =
                    if stderr_text.contains("No such file") || stderr_text.contains("not found") {
                        Some("Ensure kubectl/helm is installed and available in PATH.".to_string())
                    } else {
                        None
                    };
                let mut err =
                    ToolError::internal("render failed").with_details(serde_json::json!({
                        "command": command,
                        "args": argv,
                        "stderr": stderr_text,
                    }));
                if let Some(hint) = hint {
                    err = err.with_hint(hint);
                }
                return Err(err);
            }
            stdout
        };

        let truncated = rendered_bytes.len() > max_capture_bytes;
        let captured = if truncated {
            &rendered_bytes[..max_capture_bytes.min(rendered_bytes.len())]
        } else {
            rendered_bytes.as_slice()
        };
        let render_sha256 = sha256_hex(captured);

        let mut render_ref = Value::Null;
        if let Some(root) = resolve_context_root() {
            let trace_id = args.get("trace_id").and_then(|v| v.as_str());
            let span_id = args.get("span_id").and_then(|v| v.as_str());
            if let Ok(reference) = build_tool_call_file_ref(trace_id, span_id, "render.yaml") {
                let content = String::from_utf8_lossy(captured).to_string();
                let written = write_text_artifact(&root, &reference, &content)?;
                render_ref = Value::String(written.uri);
            }
        }

        let rendered = String::from_utf8_lossy(captured).to_string();
        let mut images: HashMap<String, u64> = HashMap::new();
        for cap in IMAGE_LINE_RE.captures_iter(&rendered) {
            if let Some(raw) = cap.name("image").map(|m| m.as_str()) {
                let img = strip_yaml_scalar(raw);
                if img.is_empty() {
                    continue;
                }
                let count = images.entry(img).or_insert(0);
                *count += 1;
            }
        }

        let mut images_items = Vec::new();
        let mut unpinned = 0u64;
        let mut latest = 0u64;
        for (img, count) in images.iter() {
            let pinned_by_digest = img.contains("@sha256:");
            let tag = image_tag(img);
            let is_latest = tag.as_deref() == Some("latest");
            if !pinned_by_digest {
                unpinned += 1;
            }
            if is_latest {
                latest += 1;
            }
            images_items.push(serde_json::json!({
                "image": img,
                "occurrences": count,
                "pinned_by_digest": pinned_by_digest,
                "tag": tag,
                "latest": is_latest,
            }));
        }
        images_items.sort_by(|a, b| {
            a.get("image")
                .and_then(|v| v.as_str())
                .cmp(&b.get("image").and_then(|v| v.as_str()))
        });

        let images_summary = serde_json::json!({
            "unique": images_items.len(),
            "total": images.values().sum::<u64>(),
            "unpinned": unpinned,
            "latest": latest,
            "truncated": truncated,
        });

        let mut images_violations = Vec::new();
        for item in images_items.iter() {
            let pinned = item
                .get("pinned_by_digest")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let is_latest = item
                .get("latest")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let image = item.get("image").cloned().unwrap_or(Value::Null);
            if !pinned {
                images_violations.push(serde_json::json!({
                    "rule": "require_digest_pinning",
                    "image": image,
                    "reason": "missing @sha256 digest",
                }));
            }
            if is_latest {
                images_violations.push(serde_json::json!({
                    "rule": "disallow_latest",
                    "image": image,
                    "reason": "tag=latest",
                }));
            }
        }

        let mut images_ref = Value::Null;
        if let Some(root) = resolve_context_root() {
            let trace_id = args.get("trace_id").and_then(|v| v.as_str());
            let span_id = args.get("span_id").and_then(|v| v.as_str());
            if let Ok(reference) = build_tool_call_file_ref(trace_id, span_id, "images.json") {
                let payload = serde_json::json!({ "images": images_items });
                let content =
                    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());
                let written = write_text_artifact(&root, &reference, &content)?;
                images_ref = Value::String(written.uri);
            }
        }

        Ok(serde_json::json!({
            "success": true,
            "render_type": render_type,
            "repo_root": repo_root,
            "overlay": overlay_path,
            "chart": chart_path,
            "values": values_paths,
            "render_ref": render_ref,
            "render_sha256": render_sha256,
            "render_bytes": rendered_bytes.len(),
            "render_captured_bytes": captured.len(),
            "render_truncated": truncated,
            "images_ref": images_ref,
            "images_summary": images_summary,
            "images_violations": images_violations,
        }))
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
        let (code, stdout, stderr, _) = self
            .run_command(&repo_root, "git", &argv, Some(30_000), None, true)
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
        let (code, stdout, stderr, _) = self
            .run_command(&repo_root, "git", &argv, Some(30_000), None, true)
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
        let (code, stdout, stderr, _) = self
            .run_command(&repo_root, "git", &argv, Some(30_000), None, true)
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
        let (code, stdout, stderr, _) = self
            .run_command(&repo_root, "git", &argv, Some(60_000), None, true)
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
