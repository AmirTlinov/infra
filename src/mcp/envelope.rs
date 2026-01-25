use crate::utils::redact::{redact_object, redact_text};
use crate::utils::text::truncate_utf8_prefix;
use serde_json::Value;

fn collect_secret_values(map: Option<&Value>) -> Option<Vec<String>> {
    let Some(Value::Object(obj)) = map else {
        return None;
    };
    let mut out = Vec::new();
    for value in obj.values() {
        if let Some(text) = value.as_str() {
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

fn present_tool_name(tool: &str, invoked_as: Option<&str>) -> String {
    if let Some(invoked) = invoked_as {
        return invoked.to_string();
    }
    match tool {
        "mcp_ssh_manager" => "ssh".to_string(),
        "mcp_artifacts" => "artifacts".to_string(),
        "mcp_jobs" => "job".to_string(),
        _ => tool.to_string(),
    }
}

fn build_ssh_next_actions(job_id: Option<&str>) -> Vec<Value> {
    let Some(job_id) = job_id else {
        return vec![];
    };
    vec![
        serde_json::json!({"tool": "job", "action": "follow_job", "args": {"job_id": job_id, "timeout_ms": 600000, "lines": 120}}),
        serde_json::json!({"tool": "job", "action": "tail_job", "args": {"job_id": job_id, "lines": 120}}),
        serde_json::json!({"tool": "job", "action": "job_cancel", "args": {"job_id": job_id}}),
    ]
}

pub fn build_generic_envelope(
    tool_name: &str,
    invoked_as: Option<&str>,
    action_name: Option<&str>,
    tool_result: &Value,
    meta: Option<&Value>,
    artifact_context_uri: Option<&str>,
    artifact_json_uri: Option<&str>,
) -> Value {
    let success = tool_result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let duration_ms = meta
        .and_then(|m| m.get("duration_ms").and_then(|v| v.as_i64()))
        .unwrap_or(0);
    let trace = serde_json::json!({
        "trace_id": meta.and_then(|m| m.get("trace_id")).cloned().unwrap_or(Value::Null),
        "span_id": meta.and_then(|m| m.get("span_id")).cloned().unwrap_or(Value::Null),
        "parent_span_id": meta.and_then(|m| m.get("parent_span_id")).cloned().unwrap_or(Value::Null),
    });
    serde_json::json!({
        "success": success,
        "tool": present_tool_name(tool_name, invoked_as),
        "action": action_name,
        "result": redact_object(tool_result, 20 * 1024, None),
        "duration_ms": duration_ms,
        "artifact_uri_context": artifact_context_uri,
        "artifact_uri_json": artifact_json_uri,
        "trace": trace,
    })
}

pub fn build_repo_exec_envelope(
    action_name: &str,
    tool_result: &Value,
    meta: Option<&Value>,
    args: &Value,
    artifact_json_uri: Option<&str>,
) -> Value {
    let exit_code = tool_result.get("exit_code").and_then(|v| v.as_i64());
    let duration_ms = meta
        .and_then(|m| m.get("duration_ms").and_then(|v| v.as_i64()))
        .unwrap_or_else(|| {
            tool_result
                .get("duration_ms")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
        });
    let timed_out = tool_result
        .get("timed_out")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let extra_secrets = collect_secret_values(args.get("env"));

    let stdout_raw = tool_result
        .get("stdout_inline")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let stderr_raw = tool_result
        .get("stderr_inline")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let stdout_redacted = redact_text(stdout_raw, usize::MAX, extra_secrets.as_deref());
    let stderr_redacted = redact_text(stderr_raw, usize::MAX, extra_secrets.as_deref());

    let stdout_bound = truncate_utf8_prefix(&stdout_redacted, 32 * 1024);
    let stderr_bound = truncate_utf8_prefix(&stderr_redacted, 16 * 1024);

    let stdout_bytes = tool_result
        .get("stdout_bytes")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let stderr_bytes = tool_result
        .get("stderr_bytes")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let stdout_truncated = tool_result
        .get("stdout_truncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || stdout_bound.len() < stdout_redacted.len();
    let stderr_truncated = tool_result
        .get("stderr_truncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || stderr_bound.len() < stderr_redacted.len();

    let success = tool_result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(exit_code == Some(0) && !timed_out);

    let mut next_actions = Vec::new();
    if stdout_truncated {
        if let Some(uri) = tool_result
            .get("stdout_ref")
            .and_then(|v| v.get("uri"))
            .and_then(|v| v.as_str())
        {
            next_actions.push(serde_json::json!({"tool": "artifacts", "action": "tail", "args": {"uri": uri, "max_bytes": 64 * 1024}}));
        }
    }
    if stderr_truncated {
        if let Some(uri) = tool_result
            .get("stderr_ref")
            .and_then(|v| v.get("uri"))
            .and_then(|v| v.as_str())
        {
            next_actions.push(serde_json::json!({"tool": "artifacts", "action": "tail", "args": {"uri": uri, "max_bytes": 64 * 1024}}));
        }
    }

    let summary = if let Some(code) = exit_code {
        format!("exit {}, {}ms", code, duration_ms)
    } else {
        format!("exit ?, {}ms", duration_ms)
    };

    serde_json::json!({
        "success": success,
        "tool": "repo",
        "action": action_name,
        "mode": "sync",
        "exit_code": exit_code,
        "timed_out": timed_out,
        "duration_ms": duration_ms,
        "stdout": stdout_bound,
        "stderr": stderr_bound,
        "stdout_bytes": stdout_bytes,
        "stderr_bytes": stderr_bytes,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
        "job_id": Value::Null,
        "next_actions": next_actions,
        "trace": {
            "trace_id": meta.and_then(|m| m.get("trace_id")).cloned().unwrap_or(Value::Null),
            "span_id": meta.and_then(|m| m.get("span_id")).cloned().unwrap_or(Value::Null),
            "parent_span_id": meta.and_then(|m| m.get("parent_span_id")).cloned().unwrap_or(Value::Null),
        },
        "summary": summary,
        "artifact_uri_json": artifact_json_uri,
    })
}

pub fn build_local_exec_envelope(
    action_name: &str,
    tool_result: &Value,
    meta: Option<&Value>,
    args: &Value,
    artifact_json_uri: Option<&str>,
) -> Value {
    let exit_code = tool_result.get("exit_code").and_then(|v| v.as_i64());
    let duration_ms = meta
        .and_then(|m| m.get("duration_ms").and_then(|v| v.as_i64()))
        .unwrap_or_else(|| {
            tool_result
                .get("duration_ms")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
        });
    let timed_out = tool_result
        .get("timed_out")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let extra_secrets = collect_secret_values(args.get("env"));
    let stdout_raw = tool_result
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let stderr_raw = tool_result
        .get("stderr")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let stdout_redacted = redact_text(stdout_raw, usize::MAX, extra_secrets.as_deref());
    let stderr_redacted = redact_text(stderr_raw, usize::MAX, extra_secrets.as_deref());
    let stdout_bound = truncate_utf8_prefix(&stdout_redacted, 32 * 1024);
    let stderr_bound = truncate_utf8_prefix(&stderr_redacted, 16 * 1024);

    let stdout_bytes = tool_result
        .get("stdout_bytes")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let stderr_bytes = tool_result
        .get("stderr_bytes")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let stdout_truncated = tool_result
        .get("stdout_inline_truncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || stdout_bound.len() < stdout_redacted.len();
    let stderr_truncated = tool_result
        .get("stderr_inline_truncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || stderr_bound.len() < stderr_redacted.len();

    let success = tool_result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(exit_code == Some(0) && !timed_out);

    let mut next_actions = Vec::new();
    if stdout_truncated {
        if let Some(path) = tool_result.get("stdout_path").and_then(|v| v.as_str()) {
            next_actions.push(serde_json::json!({"tool": "local", "action": "fs_read", "args": {"path": path, "encoding": "utf8", "offset": 0, "length": 64 * 1024}}));
        }
    }
    if stderr_truncated {
        if let Some(path) = tool_result.get("stderr_path").and_then(|v| v.as_str()) {
            next_actions.push(serde_json::json!({"tool": "local", "action": "fs_read", "args": {"path": path, "encoding": "utf8", "offset": 0, "length": 64 * 1024}}));
        }
    }

    let summary = if let Some(code) = exit_code {
        format!("exit {}, {}ms", code, duration_ms)
    } else {
        format!("exit ?, {}ms", duration_ms)
    };

    serde_json::json!({
        "success": success,
        "tool": "local",
        "action": action_name,
        "mode": "sync",
        "exit_code": exit_code,
        "timed_out": timed_out,
        "duration_ms": duration_ms,
        "stdout": stdout_bound,
        "stderr": stderr_bound,
        "stdout_bytes": stdout_bytes,
        "stderr_bytes": stderr_bytes,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
        "job_id": Value::Null,
        "next_actions": next_actions,
        "trace": {
            "trace_id": meta.and_then(|m| m.get("trace_id")).cloned().unwrap_or(Value::Null),
            "span_id": meta.and_then(|m| m.get("span_id")).cloned().unwrap_or(Value::Null),
            "parent_span_id": meta.and_then(|m| m.get("parent_span_id")).cloned().unwrap_or(Value::Null),
        },
        "summary": summary,
        "artifact_uri_json": artifact_json_uri,
    })
}

pub fn build_ssh_exec_envelope(
    action_name: &str,
    tool_result: &Value,
    meta: Option<&Value>,
    args: &Value,
    artifact_json_uri: Option<&str>,
) -> Value {
    let job_id = tool_result.get("job_id").and_then(|v| v.as_str());
    let is_follow = tool_result.get("start").is_some() && tool_result.get("wait").is_some();
    let mode = if action_name == "exec_detached"
        || tool_result
            .get("detached")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        || job_id.is_some()
    {
        "detached"
    } else {
        "sync"
    };
    let exit_code = if mode == "sync" {
        tool_result.get("exitCode").and_then(|v| v.as_i64())
    } else {
        tool_result
            .get("status")
            .and_then(|v| v.get("exit_code"))
            .and_then(|v| v.as_i64())
    };
    let requested_timeout = tool_result
        .get("requested_timeout_ms")
        .and_then(|v| v.as_i64())
        .or_else(|| args.get("timeout_ms").and_then(|v| v.as_i64()));
    let timed_out = if mode == "sync" {
        tool_result
            .get("timedOut")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || tool_result
                .get("hardTimedOut")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
    } else if is_follow {
        tool_result
            .get("wait")
            .and_then(|v| v.get("timed_out"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    } else {
        tool_result
            .get("timedOut")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || tool_result
                .get("hardTimedOut")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
    };
    let duration_ms = meta
        .and_then(|m| m.get("duration_ms").and_then(|v| v.as_i64()))
        .or_else(|| tool_result.get("duration_ms").and_then(|v| v.as_i64()))
        .unwrap_or(0);

    let extra_secrets = collect_secret_values(args.get("env"));
    let stdout_raw = if mode == "sync" {
        tool_result
            .get("stdout")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    } else if is_follow {
        tool_result
            .get("logs")
            .and_then(|v| v.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
    } else {
        tool_result
            .get("stdout")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    };
    let stderr_raw = tool_result
        .get("stderr")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let stdout_redacted = redact_text(stdout_raw, usize::MAX, extra_secrets.as_deref());
    let stderr_redacted = redact_text(stderr_raw, usize::MAX, extra_secrets.as_deref());
    let stdout_bound = truncate_utf8_prefix(&stdout_redacted, 32 * 1024);
    let stderr_bound = truncate_utf8_prefix(&stderr_redacted, 16 * 1024);

    let stdout_bytes = if mode == "sync" {
        tool_result
            .get("stdout_bytes")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
    } else {
        0
    };
    let stderr_bytes = if mode == "sync" {
        tool_result
            .get("stderr_bytes")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
    } else {
        0
    };

    let stdout_truncated = if mode == "sync" {
        tool_result
            .get("stdout_truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || tool_result
                .get("stdout_inline_truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            || stdout_bound.len() < stdout_redacted.len()
    } else {
        false
    };
    let stderr_truncated = if mode == "sync" {
        tool_result
            .get("stderr_truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || tool_result
                .get("stderr_inline_truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            || stderr_bound.len() < stderr_redacted.len()
    } else {
        false
    };

    let success = tool_result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(exit_code == Some(0) && !timed_out);

    let wait_completed = if mode == "detached" && is_follow {
        tool_result
            .get("wait")
            .and_then(|v| v.get("completed"))
            .and_then(|v| v.as_bool())
    } else {
        None
    };
    let wait_waited_ms = if mode == "detached" && is_follow {
        tool_result
            .get("wait")
            .and_then(|v| v.get("waited_ms"))
            .and_then(|v| v.as_i64())
    } else {
        None
    };

    let summary = if mode == "sync" && exit_code.is_some() {
        format!("exit {}, {}ms", exit_code.unwrap(), duration_ms)
    } else if mode == "detached" && exit_code.is_some() && wait_completed == Some(true) {
        format!("exit {} (follow), {}ms", exit_code.unwrap(), duration_ms)
    } else if mode == "detached" && timed_out && wait_waited_ms.is_some() {
        format!(
            "running (follow timed out after {}ms)",
            wait_waited_ms.unwrap()
        )
    } else if duration_ms > 0 {
        format!("detached, {}ms", duration_ms)
    } else if exit_code.is_some() {
        format!("exit {}", exit_code.unwrap())
    } else {
        "detached".to_string()
    };

    let mut next_actions = build_ssh_next_actions(job_id);
    if mode == "sync" {
        if stdout_truncated {
            if let Some(uri) = tool_result
                .get("stdout_ref")
                .and_then(|v| v.get("uri"))
                .and_then(|v| v.as_str())
            {
                next_actions.push(serde_json::json!({"tool": "artifacts", "action": "tail", "args": {"uri": uri, "max_bytes": 64 * 1024}}));
            }
        }
        if stderr_truncated {
            if let Some(uri) = tool_result
                .get("stderr_ref")
                .and_then(|v| v.get("uri"))
                .and_then(|v| v.as_str())
            {
                next_actions.push(serde_json::json!({"tool": "artifacts", "action": "tail", "args": {"uri": uri, "max_bytes": 64 * 1024}}));
            }
        }
    }

    serde_json::json!({
        "success": success,
        "tool": "ssh",
        "action": action_name,
        "mode": mode,
        "exit_code": exit_code,
        "timed_out": timed_out,
        "duration_ms": duration_ms,
        "stdout": stdout_bound,
        "stderr": stderr_bound,
        "stdout_bytes": stdout_bytes,
        "stderr_bytes": stderr_bytes,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
        "job_id": if mode == "detached" { job_id } else { None },
        "wait": if mode == "detached" && is_follow { tool_result.get("wait").cloned().unwrap_or(Value::Null) } else { Value::Null },
        "status": if mode == "detached" && is_follow { tool_result.get("status").cloned().unwrap_or(Value::Null) } else { Value::Null },
        "next_actions": next_actions,
        "trace": {
            "trace_id": meta.and_then(|m| m.get("trace_id")).cloned().unwrap_or(Value::Null),
            "span_id": meta.and_then(|m| m.get("span_id")).cloned().unwrap_or(Value::Null),
            "parent_span_id": meta.and_then(|m| m.get("parent_span_id")).cloned().unwrap_or(Value::Null),
        },
        "summary": summary,
        "artifact_uri_json": artifact_json_uri,
        "requested_timeout_ms": requested_timeout,
    })
}
