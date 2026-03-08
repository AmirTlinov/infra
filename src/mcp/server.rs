use crate::app::App;
use crate::constants::timeouts::IDLE_TIMEOUT_MS;
use crate::errors::{ErrorCode, McpError, ToolError};
use crate::mcp::aliases::canonical_tool_name;
use crate::mcp::catalog::{list_tools_for_openai, tool_by_name, validate_tool_args};
use crate::mcp::envelope::{
    build_generic_envelope, build_local_exec_envelope, build_repo_exec_envelope,
    build_ssh_exec_envelope,
};
use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::mcp::{help, legend, prompts, resources};
use crate::services::tool_executor::ToolCallMeta;
use crate::utils::arg_aliases::normalize_args_aliases;
use crate::utils::artifacts::{
    build_tool_call_context_ref, build_tool_call_file_ref, resolve_context_root,
    write_text_artifact,
};
use crate::utils::redact::redact_text;
use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufRead as _, ErrorKind};
use std::sync::Arc;
use std::time::Duration;
#[cfg(test)]
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};
use tokio::sync::mpsc;

const PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "infra";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const STDIO_IDLE_TIMEOUT_ENV: &str = "INFRA_STDIO_IDLE_TIMEOUT_MS";

fn core_tool_names() -> HashSet<String> {
    HashSet::from([
        "help".to_string(),
        "legend".to_string(),
        "mcp_capability".to_string(),
        "mcp_operation".to_string(),
        "mcp_receipt".to_string(),
        "mcp_policy".to_string(),
        "mcp_profile".to_string(),
        "mcp_target".to_string(),
    ])
}

fn resolve_tool_tier() -> String {
    let raw = std::env::var("INFRA_TOOL_TIER").unwrap_or_else(|_| "core".to_string());
    let normalized = raw.trim().to_lowercase();
    if normalized == "core" {
        "core".to_string()
    } else if normalized == "expert" || normalized == "full" {
        "expert".to_string()
    } else {
        "core".to_string()
    }
}

fn is_graceful_stdio_disconnect(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::BrokenPipe
            | ErrorKind::UnexpectedEof
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
    )
}

#[derive(Debug, PartialEq, Eq)]
enum ReadLoopEvent {
    Line(String),
    Eof,
    IdleTimeout,
}

fn resolve_stdio_idle_timeout_ms() -> Option<u64> {
    match std::env::var(STDIO_IDLE_TIMEOUT_ENV) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(value) => Some(value),
            Err(_) => Some(IDLE_TIMEOUT_MS),
        },
        Err(_) => Some(IDLE_TIMEOUT_MS),
    }
}

#[cfg(test)]
async fn read_loop_event<R>(
    reader: &mut tokio::io::Lines<BufReader<R>>,
    idle_timeout_ms: Option<u64>,
) -> Result<ReadLoopEvent, ToolError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let next_line = async {
        reader
            .next_line()
            .await
            .map_err(|err| ToolError::internal(err.to_string()))
    };

    let line = if let Some(timeout_ms) = idle_timeout_ms {
        match tokio::time::timeout(Duration::from_millis(timeout_ms), next_line).await {
            Ok(result) => result?,
            Err(_) => return Ok(ReadLoopEvent::IdleTimeout),
        }
    } else {
        next_line.await?
    };

    Ok(match line {
        Some(line) => ReadLoopEvent::Line(line),
        None => ReadLoopEvent::Eof,
    })
}

async fn channel_read_loop_event(
    receiver: &mut mpsc::Receiver<Result<ReadLoopEvent, ToolError>>,
    idle_timeout_ms: Option<u64>,
) -> Result<ReadLoopEvent, ToolError> {
    let next_event = async { receiver.recv().await };

    let event = if let Some(timeout_ms) = idle_timeout_ms {
        match tokio::time::timeout(Duration::from_millis(timeout_ms), next_event).await {
            Ok(event) => event,
            Err(_) => return Ok(ReadLoopEvent::IdleTimeout),
        }
    } else {
        next_event.await
    };

    match event {
        Some(result) => result,
        None => Ok(ReadLoopEvent::Eof),
    }
}

fn spawn_stdio_reader_thread() -> Result<mpsc::Receiver<Result<ReadLoopEvent, ToolError>>, ToolError>
{
    let (tx, rx) = mpsc::channel(1);
    std::thread::Builder::new()
        .name("infra-stdio-reader".to_string())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();

            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        let _ = tx.blocking_send(Ok(ReadLoopEvent::Eof));
                        break;
                    }
                    Ok(_) => {
                        let line = line.trim_end_matches(['\r', '\n']).to_string();
                        if tx.blocking_send(Ok(ReadLoopEvent::Line(line))).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        let _ = tx.blocking_send(Err(ToolError::internal(err.to_string())));
                        break;
                    }
                }
            }
        })
        .map_err(|err| {
            ToolError::internal(format!("failed to spawn stdio reader thread: {err}"))
        })?;
    Ok(rx)
}

async fn write_jsonrpc_response<W: AsyncWrite + Unpin>(
    writer: &mut BufWriter<W>,
    response: &JsonRpcResponse,
) -> Result<bool, ToolError> {
    let payload = serde_json::to_string(response).unwrap_or_default();
    if let Err(err) = writer.write_all(payload.as_bytes()).await {
        if is_graceful_stdio_disconnect(&err) {
            return Ok(false);
        }
        return Err(err.into());
    }
    if let Err(err) = writer.write_all(b"\n").await {
        if is_graceful_stdio_disconnect(&err) {
            return Ok(false);
        }
        return Err(err.into());
    }
    if let Err(err) = writer.flush().await {
        if is_graceful_stdio_disconnect(&err) {
            return Ok(false);
        }
        return Err(err.into());
    }
    Ok(true)
}

fn normalize_response_mode(value: &Value) -> Result<Option<String>, McpError> {
    if value.is_null() {
        return Ok(None);
    }
    let raw = if let Some(text) = value.as_str() {
        text.to_string()
    } else {
        value.to_string()
    };
    let normalized = raw.trim().to_lowercase();
    if normalized.is_empty() {
        return Ok(None);
    }
    if normalized == "ai" || normalized == "compact" {
        return Ok(Some(normalized));
    }
    Err(McpError::new(
        ErrorCode::InvalidParams,
        "response_mode: expected one of ai, compact",
    ))
}

fn resolve_response_mode(args: &Value) -> Result<String, McpError> {
    if let Some(obj) = args.as_object() {
        if obj.contains_key("response_mode") {
            let mode = normalize_response_mode(obj.get("response_mode").unwrap())?;
            return Ok(mode.unwrap_or_else(|| "ai".to_string()));
        }
    }
    Ok("ai".to_string())
}

fn strip_response_mode(args: &Value) -> Value {
    if let Some(obj) = args.as_object() {
        if obj.contains_key("response_mode") {
            let mut out = obj.clone();
            out.remove("response_mode");
            return Value::Object(out);
        }
    }
    args.clone()
}

fn map_tool_error(tool: &str, error: &ToolError) -> McpError {
    let mut lines = vec![
        "InfraError".to_string(),
        format!("tool: {}", tool),
        format!("kind: {:?}", error.kind).to_lowercase(),
        format!("code: {}", error.code),
        format!("retryable: {}", error.retryable),
        format!("message: {}", error.message),
    ];
    if let Some(hint) = &error.hint {
        lines.push(format!("hint: {}", hint));
    }
    let message = lines.join("\n");

    match error.kind {
        crate::errors::ToolErrorKind::InvalidParams => {
            McpError::new(ErrorCode::InvalidParams, message)
        }
        crate::errors::ToolErrorKind::Timeout => McpError::new(ErrorCode::RequestTimeout, message),
        crate::errors::ToolErrorKind::Denied
        | crate::errors::ToolErrorKind::Conflict
        | crate::errors::ToolErrorKind::NotFound => {
            McpError::new(ErrorCode::InvalidRequest, message)
        }
        _ => McpError::new(ErrorCode::InternalError, message),
    }
}

fn build_context_header() -> Vec<String> {
    vec![
        "[LEGEND]".to_string(),
        "A = Answer line (1–3 lines max).".to_string(),
        "R = Reference anchor.".to_string(),
        "C = Command to verify/reproduce.".to_string(),
        "E = Error (typed, actionable).".to_string(),
        "M = Continuation marker (cursor/more).".to_string(),
        "N = Note.".to_string(),
        "".to_string(),
    ]
}

fn format_context_doc(lines: Vec<String>) -> String {
    let mut out = lines.join("\n");
    out = out.trim().to_string();
    out.push('\n');
    out
}

fn compact_value(
    value: &Value,
    max_depth: usize,
    max_array: usize,
    max_keys: usize,
    depth: usize,
) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    if depth >= max_depth {
        if value.is_array() {
            return Value::String(format!(
                "[array:{}]",
                value.as_array().map(|a| a.len()).unwrap_or(0)
            ));
        }
        if value.is_object() {
            return Value::String("[object]".to_string());
        }
        return value.clone();
    }
    if let Some(arr) = value.as_array() {
        let mut out = Vec::new();
        for item in arr.iter().take(max_array) {
            out.push(compact_value(
                item,
                max_depth,
                max_array,
                max_keys,
                depth + 1,
            ));
        }
        if arr.len() > max_array {
            out.push(Value::String(format!(
                "[... +{} more]",
                arr.len() - max_array
            )));
        }
        return Value::Array(out);
    }
    if let Some(obj) = value.as_object() {
        let mut out = serde_json::Map::new();
        for (idx, (key, val)) in obj.iter().enumerate() {
            if idx >= max_keys {
                out.insert(
                    "__more_keys__".to_string(),
                    Value::Number(serde_json::Number::from(obj.len() - max_keys)),
                );
                break;
            }
            out.insert(
                key.clone(),
                compact_value(val, max_depth, max_array, max_keys, depth + 1),
            );
        }
        return Value::Object(out);
    }
    value.clone()
}

fn collect_artifact_refs(value: &Value, max_refs: usize, max_depth: usize) -> Vec<String> {
    let mut refs = Vec::new();
    let mut stack = vec![(value, 0usize)];
    let mut seen = HashSet::new();
    while let Some((node, depth)) = stack.pop() {
        if refs.len() >= max_refs {
            break;
        }
        if let Some(text) = node.as_str() {
            let trimmed = text.trim();
            if trimmed.starts_with("artifact://") && !seen.contains(trimmed) {
                seen.insert(trimmed.to_string());
                refs.push(trimmed.to_string());
            }
            continue;
        }
        if depth >= max_depth {
            continue;
        }
        if let Some(arr) = node.as_array() {
            for item in arr.iter().rev() {
                stack.push((item, depth + 1));
            }
            continue;
        }
        if let Some(obj) = node.as_object() {
            for val in obj.values().rev() {
                stack.push((val, depth + 1));
            }
        }
    }
    refs
}

fn format_generic_result_to_context(
    tool: &str,
    action: Option<&str>,
    result: &Value,
    meta: Option<&Value>,
    artifact_uri: Option<&str>,
    artifact_write_error: Option<&str>,
) -> String {
    let mut lines = build_context_header();
    lines.push("[CONTENT]".to_string());
    let action_text = action.unwrap_or("");
    lines.push(
        format!("A: {} {}", tool, action_text)
            .trim_end()
            .to_string(),
    );

    if let Some(meta) = meta {
        if let Some(duration) = meta.get("duration_ms").and_then(|v| v.as_i64()) {
            lines.push(format!("N: duration_ms: {}", duration));
        }
    }

    let compacted = compact_value(result, 6, 50, 50, 0);
    match compacted {
        Value::Array(arr) => {
            lines.push(format!("N: result: array (length: {})", arr.len()));
        }
        Value::Object(obj) => {
            let keys: Vec<String> = obj.keys().take(12).cloned().collect();
            lines.push(format!(
                "N: result: object (keys: {}{})",
                keys.join(", "),
                if obj.len() > keys.len() { ", ..." } else { "" }
            ));
        }
        _ => {
            lines.push(format!(
                "N: result: {}",
                redact_text(&compacted.to_string(), 2048, None)
            ));
        }
    }

    if let Some(uri) = artifact_uri {
        lines.push(format!("R: {}", uri));
    }

    if let Some(err) = artifact_write_error {
        lines.push(format!("N: artifact_write_failed: {}", err));
    }

    let refs = collect_artifact_refs(result, 10, 8);
    if !refs.is_empty() {
        lines.push("N: referenced_artifacts:".to_string());
        for reference in refs {
            lines.push(format!("R: {}", reference));
        }
    }

    format_context_doc(lines)
}

fn format_help_result_to_context(result: &Value) -> String {
    let mut lines = build_context_header();
    lines.push("[CONTENT]".to_string());
    lines.push("A: help".to_string());
    if let Some(obj) = result.as_object() {
        if let Some(overview) = obj.get("overview").and_then(|v| v.as_str()) {
            lines.push(format!("N: {}", overview));
        }
        if let Some(tools) = obj.get("tools").and_then(|v| v.as_array()) {
            lines.push(format!("N: tools: {}", tools.len()));
        }
    }
    format_context_doc(lines)
}

fn format_legend_result_to_context(result: &Value) -> String {
    let mut lines = build_context_header();
    lines.push("[CONTENT]".to_string());
    lines.push("A: legend".to_string());
    if let Some(obj) = result.as_object() {
        if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
            lines.push(format!("N: name: {}", name));
        }
    }
    format_context_doc(lines)
}

pub struct McpServer {
    app: Arc<App>,
}

impl McpServer {
    pub async fn new() -> Result<Self, ToolError> {
        let app = App::initialize()?;
        Ok(Self { app: Arc::new(app) })
    }

    async fn handle_initialize(&self) -> Value {
        serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "tools": {"list": true, "call": true},
                "resources": {"list": true, "read": true},
                "prompts": {"list": true, "get": true},
            },
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
        })
    }

    async fn handle_tools_list(&self) -> Value {
        let tier = resolve_tool_tier();
        let tools = list_tools_for_openai(&tier, &core_tool_names());
        serde_json::json!({ "tools": tools })
    }

    async fn handle_resources_list(&self) -> Value {
        resources::list_resources()
    }

    async fn handle_resources_read(&self, uri: &str) -> Result<Value, McpError> {
        resources::read_resource(&self.app, uri)
            .await
            .map_err(|err| McpError::new(ErrorCode::InvalidParams, err.message))
    }

    async fn handle_prompts_list(&self) -> Value {
        prompts::list_prompts()
    }

    async fn handle_prompts_get(&self, name: &str, arguments: Value) -> Result<Value, McpError> {
        prompts::get_prompt(name, &arguments).ok_or_else(|| {
            McpError::new(
                ErrorCode::InvalidParams,
                format!("Unknown prompt: {}", name),
            )
        })
    }

    async fn handle_tools_call(&self, name: &str, raw_args: Value) -> Result<Value, McpError> {
        let response_mode = resolve_response_mode(&raw_args)?;
        let mut args = strip_response_mode(&raw_args);
        let canonical_tool = canonical_tool_name(name);

        if response_mode == "ai" || response_mode == "compact" {
            if let Some(obj) = args.as_object() {
                let is_exec = obj.get("action").and_then(|v| v.as_str()) == Some("exec");
                let wants_inline_default =
                    (canonical_tool == "mcp_repo" || canonical_tool == "mcp_local") && is_exec;
                if wants_inline_default && !obj.contains_key("inline") {
                    let mut next = obj.clone();
                    next.insert("inline".to_string(), Value::Bool(true));
                    args = Value::Object(next);
                }
            }
        }

        let mut normalization = None;
        if let Some(obj) = args.as_object() {
            let allowed_keys = tool_by_name(canonical_tool)
                .and_then(|tool| tool.input_schema.get("properties"))
                .and_then(|props| props.as_object())
                .map(|map| map.keys().cloned().collect::<HashSet<String>>());
            let action = obj.get("action").and_then(|v| v.as_str());
            let (normalized_args, norm) = normalize_args_aliases(
                &Value::Object(obj.clone()),
                canonical_tool,
                action,
                allowed_keys.as_ref(),
            );
            args = normalized_args;
            normalization = norm;
        }

        validate_tool_args(canonical_tool, &args)?;

        let trace_id = args
            .get("trace_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let span_id = args
            .get("span_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let parent_span_id = args
            .get("parent_span_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let started_at = chrono::Utc::now().timestamp_millis();
        let payload = if name == "help" {
            let result = help::build_help_payload(&self.app, &args);
            self.app
                .tool_executor
                .wrap_result(
                    name,
                    &args,
                    &result,
                    ToolCallMeta {
                        started_at,
                        trace_id: trace_id.clone(),
                        span_id: span_id.clone(),
                        parent_span_id: parent_span_id.clone(),
                        invoked_as: None,
                    },
                )
                .await
                .map_err(|err| map_tool_error(name, &err))?
        } else if name == "legend" {
            let result = legend::build_legend_payload();
            self.app
                .tool_executor
                .wrap_result(
                    name,
                    &args,
                    &result,
                    ToolCallMeta {
                        started_at,
                        trace_id: trace_id.clone(),
                        span_id: span_id.clone(),
                        parent_span_id: parent_span_id.clone(),
                        invoked_as: None,
                    },
                )
                .await
                .map_err(|err| map_tool_error(name, &err))?
        } else {
            self.app
                .tool_executor
                .execute(name, args.clone())
                .await
                .map_err(|err| map_tool_error(name, &err))?
        };

        let meta = payload.get("meta").cloned();
        let tool_result = payload.get("result").cloned().unwrap_or(payload.clone());

        let context_root = resolve_context_root();
        let mut artifact_context = None;
        let mut artifact_json = None;
        if let (Some(root), Some(meta)) = (context_root.as_ref(), meta.as_ref()) {
            let trace_id = meta.get("trace_id").and_then(|v| v.as_str());
            let span_id = meta.get("span_id").and_then(|v| v.as_str());
            if let (Ok(ctx_ref), Ok(json_ref)) = (
                build_tool_call_context_ref(trace_id, span_id),
                build_tool_call_file_ref(trace_id, span_id, "result.json"),
            ) {
                artifact_context = Some(ctx_ref);
                artifact_json = Some(json_ref);
            }
            let _ = root;
        }

        let tool_name = meta
            .as_ref()
            .and_then(|m| m.get("tool").and_then(|v| v.as_str()))
            .unwrap_or(name);
        let action_name = meta
            .as_ref()
            .and_then(|m| m.get("action").and_then(|v| v.as_str()))
            .or_else(|| args.get("action").and_then(|v| v.as_str()));

        let mut artifact_write_error = None;
        if let (Some(root), Some(reference)) = (context_root.as_ref(), artifact_context.as_ref()) {
            let text = if tool_name == "help" {
                format_help_result_to_context(&tool_result)
            } else if tool_name == "legend" {
                format_legend_result_to_context(&tool_result)
            } else {
                format_generic_result_to_context(
                    tool_name,
                    action_name,
                    &tool_result,
                    meta.as_ref(),
                    Some(&reference.uri),
                    None,
                )
            };
            if let Err(err) = write_text_artifact(root, reference, &text) {
                artifact_write_error = Some(err.to_string());
            }
        }

        let envelope = if tool_name == "mcp_ssh_manager"
            && matches!(
                action_name,
                Some("exec") | Some("exec_detached") | Some("exec_follow")
            ) {
            build_ssh_exec_envelope(
                action_name.unwrap_or("exec"),
                &tool_result,
                meta.as_ref(),
                &args,
                artifact_json.as_ref().map(|r| r.uri.as_str()),
            )
        } else if tool_name == "mcp_repo" && action_name == Some("exec") {
            build_repo_exec_envelope(
                action_name.unwrap_or("exec"),
                &tool_result,
                meta.as_ref(),
                &args,
                artifact_json.as_ref().map(|r| r.uri.as_str()),
            )
        } else if tool_name == "mcp_local" && action_name == Some("exec") {
            build_local_exec_envelope(
                action_name.unwrap_or("exec"),
                &tool_result,
                meta.as_ref(),
                &args,
                artifact_json.as_ref().map(|r| r.uri.as_str()),
            )
        } else {
            build_generic_envelope(
                tool_name,
                meta.as_ref()
                    .and_then(|m| m.get("invoked_as").and_then(|v| v.as_str())),
                action_name,
                &tool_result,
                meta.as_ref(),
                artifact_context.as_ref().map(|r| r.uri.as_str()),
                artifact_json.as_ref().map(|r| r.uri.as_str()),
            )
        };

        let mut envelope = envelope;
        if let (Some(err), Some(ctx)) = (artifact_write_error.as_ref(), artifact_context.as_ref()) {
            if let Some(obj) = envelope.as_object_mut() {
                obj.insert(
                    "artifact_uri_context".to_string(),
                    Value::String(ctx.uri.clone()),
                );
                obj.insert(
                    "artifact_write_failed".to_string(),
                    Value::String(err.clone()),
                );
            }
        }

        if let Some(normalization) = normalization {
            if let Some(obj) = envelope.as_object_mut() {
                obj.insert("normalization".to_string(), normalization);
            }
        }

        if let (Some(root), Some(reference)) = (context_root.as_ref(), artifact_json.as_ref()) {
            if write_text_artifact(
                root,
                reference,
                &serde_json::to_string(&envelope).unwrap_or_default(),
            )
            .is_err()
            {
                if let Some(obj) = envelope.as_object_mut() {
                    obj.insert("artifact_uri_json".to_string(), Value::Null);
                }
            }
        }

        Ok(serde_json::json!({
            "structuredContent": envelope,
            "content": [ { "type": "text", "text": serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string()) } ]
        }))
    }

    #[cfg(test)]
    async fn run_with_io<R, W>(
        &self,
        reader: R,
        writer: W,
        idle_timeout_ms: Option<u64>,
    ) -> Result<(), ToolError>
    where
        R: tokio::io::AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut reader = BufReader::new(reader).lines();
        let mut writer = BufWriter::new(writer);

        loop {
            match read_loop_event(&mut reader, idle_timeout_ms).await? {
                ReadLoopEvent::Line(line) => {
                    if !self.process_jsonrpc_line(&mut writer, line).await? {
                        return Ok(());
                    }
                }
                ReadLoopEvent::Eof => break,
                ReadLoopEvent::IdleTimeout => {
                    eprintln!(
                        "infra: stdio idle timeout reached ({} ms), exiting",
                        idle_timeout_ms.unwrap_or(0)
                    );
                    break;
                }
            }
        }

        Ok(())
    }

    async fn run_with_stdio_receiver<W>(
        &self,
        mut receiver: mpsc::Receiver<Result<ReadLoopEvent, ToolError>>,
        writer: W,
        idle_timeout_ms: Option<u64>,
    ) -> Result<bool, ToolError>
    where
        W: AsyncWrite + Unpin,
    {
        let mut writer = BufWriter::new(writer);

        loop {
            match channel_read_loop_event(&mut receiver, idle_timeout_ms).await? {
                ReadLoopEvent::Line(line) => {
                    if !self.process_jsonrpc_line(&mut writer, line).await? {
                        return Ok(false);
                    }
                }
                ReadLoopEvent::Eof => break,
                ReadLoopEvent::IdleTimeout => {
                    eprintln!(
                        "infra: stdio idle timeout reached ({} ms), exiting",
                        idle_timeout_ms.unwrap_or(0)
                    );
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    async fn process_jsonrpc_line<W: AsyncWrite + Unpin>(
        &self,
        writer: &mut BufWriter<W>,
        line: String,
    ) -> Result<bool, ToolError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(true);
        }

        let parsed: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => {
                let response = JsonRpcResponse::failure(
                    Value::Null,
                    ErrorCode::ParseError.as_i32(),
                    "Parse error".to_string(),
                );
                return write_jsonrpc_response(writer, &response).await;
            }
        };

        let request: JsonRpcRequest = match serde_json::from_value(parsed) {
            Ok(req) => req,
            Err(_) => {
                let response = JsonRpcResponse::failure(
                    Value::Null,
                    ErrorCode::InvalidRequest.as_i32(),
                    "Invalid request".to_string(),
                );
                return write_jsonrpc_response(writer, &response).await;
            }
        };

        let response = match request.method.as_str() {
            "notifications/initialized" => request
                .id
                .clone()
                .map(|id| JsonRpcResponse::success(id, serde_json::json!({}))),
            _ if request.method.starts_with("notifications/") && request.id.is_none() => None,
            "initialize" => match request.id.clone() {
                Some(id) => Some(JsonRpcResponse::success(id, self.handle_initialize().await)),
                None => None,
            },
            "tools/list" => match request.id.clone() {
                Some(id) => Some(JsonRpcResponse::success(id, self.handle_tools_list().await)),
                None => None,
            },
            "tools/call" => match request.id.clone() {
                Some(id) => {
                    let params = request.params.as_object().cloned().unwrap_or_default();
                    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if name.is_empty() {
                        Some(JsonRpcResponse::failure(
                            id,
                            ErrorCode::InvalidParams.as_i32(),
                            "Missing tool name".to_string(),
                        ))
                    } else {
                        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                        let call = match self.handle_tools_call(name, args).await {
                            Ok(result) => JsonRpcResponse::success(id, result),
                            Err(err) => {
                                JsonRpcResponse::failure(id, err.code.as_i32(), err.message)
                            }
                        };
                        Some(call)
                    }
                }
                None => None,
            },
            "resources/list" => match request.id.clone() {
                Some(id) => Some(JsonRpcResponse::success(
                    id,
                    self.handle_resources_list().await,
                )),
                None => None,
            },
            "resources/read" => match request.id.clone() {
                Some(id) => {
                    let params = request.params.as_object().cloned().unwrap_or_default();
                    let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
                    if uri.is_empty() {
                        Some(JsonRpcResponse::failure(
                            id,
                            ErrorCode::InvalidParams.as_i32(),
                            "Missing resource uri".to_string(),
                        ))
                    } else {
                        let result = match self.handle_resources_read(uri).await {
                            Ok(value) => JsonRpcResponse::success(id, value),
                            Err(err) => {
                                JsonRpcResponse::failure(id, err.code.as_i32(), err.message)
                            }
                        };
                        Some(result)
                    }
                }
                None => None,
            },
            "prompts/list" => match request.id.clone() {
                Some(id) => Some(JsonRpcResponse::success(
                    id,
                    self.handle_prompts_list().await,
                )),
                None => None,
            },
            "prompts/get" => match request.id.clone() {
                Some(id) => {
                    let params = request.params.as_object().cloned().unwrap_or_default();
                    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if name.is_empty() {
                        Some(JsonRpcResponse::failure(
                            id,
                            ErrorCode::InvalidParams.as_i32(),
                            "Missing prompt name".to_string(),
                        ))
                    } else {
                        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
                        let result = match self.handle_prompts_get(name, arguments).await {
                            Ok(value) => JsonRpcResponse::success(id, value),
                            Err(err) => {
                                JsonRpcResponse::failure(id, err.code.as_i32(), err.message)
                            }
                        };
                        Some(result)
                    }
                }
                None => None,
            },
            _ => request.id.clone().map(|id| {
                JsonRpcResponse::failure(
                    id,
                    ErrorCode::MethodNotFound.as_i32(),
                    "Method not found".to_string(),
                )
            }),
        };

        if let Some(response) = response {
            return write_jsonrpc_response(writer, &response).await;
        }

        Ok(true)
    }

    pub async fn run_stdio(&self) -> Result<(), ToolError> {
        let stdin_events = spawn_stdio_reader_thread()?;
        let stdout = tokio::io::stdout();
        let idle_timed_out = self
            .run_with_stdio_receiver(stdin_events, stdout, resolve_stdio_idle_timeout_ms())
            .await?;
        if idle_timed_out {
            std::process::exit(0);
        }
        Ok(())
    }
}

pub async fn run_stdio() -> Result<(), ToolError> {
    let server = McpServer::new().await?;
    server.run_stdio().await
}

#[cfg(test)]
mod tests {
    use super::{
        is_graceful_stdio_disconnect, read_loop_event, resolve_stdio_idle_timeout_ms, McpServer,
        ReadLoopEvent, SERVER_VERSION, STDIO_IDLE_TIMEOUT_ENV,
    };
    use crate::constants::timeouts::IDLE_TIMEOUT_MS;
    use once_cell::sync::Lazy;
    use serde_json::json;
    use std::io::ErrorKind;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::sync::Mutex;

    static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

    fn restore_env(key: &str, previous: Option<String>) {
        match previous {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn graceful_disconnect_kinds_are_recognized() {
        for kind in [
            ErrorKind::BrokenPipe,
            ErrorKind::UnexpectedEof,
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
        ] {
            let err = std::io::Error::from(kind);
            assert!(is_graceful_stdio_disconnect(&err));
        }
        let other = std::io::Error::from(ErrorKind::PermissionDenied);
        assert!(!is_graceful_stdio_disconnect(&other));
    }

    #[tokio::test]
    async fn stdio_read_loop_returns_idle_timeout_when_no_input_arrives() {
        let (_client, server_side) = tokio::io::duplex(64);
        let mut reader = BufReader::new(server_side).lines();
        let event = read_loop_event(&mut reader, Some(10))
            .await
            .expect("read loop event");
        assert_eq!(event, ReadLoopEvent::IdleTimeout);
    }

    #[tokio::test]
    async fn stdio_idle_timeout_env_zero_disables_timeout() {
        let _guard = ENV_LOCK.lock().await;
        let prev_timeout = std::env::var(STDIO_IDLE_TIMEOUT_ENV).ok();
        std::env::set_var(STDIO_IDLE_TIMEOUT_ENV, "0");
        assert_eq!(resolve_stdio_idle_timeout_ms(), None);
        restore_env(STDIO_IDLE_TIMEOUT_ENV, prev_timeout);
    }

    #[tokio::test]
    async fn invalid_stdio_idle_timeout_env_falls_back_to_safe_default() {
        let _guard = ENV_LOCK.lock().await;
        let prev_timeout = std::env::var(STDIO_IDLE_TIMEOUT_ENV).ok();
        std::env::set_var(STDIO_IDLE_TIMEOUT_ENV, "not-a-number");
        assert_eq!(resolve_stdio_idle_timeout_ms(), Some(IDLE_TIMEOUT_MS));
        restore_env(STDIO_IDLE_TIMEOUT_ENV, prev_timeout);
    }

    #[tokio::test]
    async fn stdio_session_finishes_active_request_then_self_terminates_on_idle() {
        let _guard = ENV_LOCK.lock().await;

        let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
        let prev_timeout = std::env::var(STDIO_IDLE_TIMEOUT_ENV).ok();
        let tmp_dir =
            std::env::temp_dir().join(format!("infra-server-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
        std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);
        std::env::set_var(STDIO_IDLE_TIMEOUT_ENV, "50");

        let server = McpServer::new().await.expect("server");
        let (mut client_writer, server_reader) = tokio::io::duplex(8 * 1024);
        let (server_writer, client_reader) = tokio::io::duplex(8 * 1024);
        let run = tokio::spawn(async move {
            server
                .run_with_io(
                    server_reader,
                    server_writer,
                    resolve_stdio_idle_timeout_ms(),
                )
                .await
        });

        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });
        client_writer
            .write_all(format!("{}\n", initialize).as_bytes())
            .await
            .expect("write initialize");

        let mut client_reader = BufReader::new(client_reader).lines();
        let response_line =
            tokio::time::timeout(Duration::from_millis(500), client_reader.next_line())
                .await
                .expect("initialize response before idle timeout")
                .expect("read initialize response")
                .expect("initialize response line");
        let response: serde_json::Value =
            serde_json::from_str(&response_line).expect("parse initialize response");
        assert_eq!(response.get("id"), Some(&serde_json::json!(1)));
        assert!(response.get("result").is_some());

        let run_result = tokio::time::timeout(Duration::from_millis(500), run)
            .await
            .expect("server exits after bounded idle")
            .expect("join run loop");
        assert!(run_result.is_ok());

        restore_env("MCP_PROFILES_DIR", prev_profiles);
        restore_env(STDIO_IDLE_TIMEOUT_ENV, prev_timeout);
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[tokio::test]
    async fn stdio_session_keeps_waiting_when_idle_timeout_is_disabled_until_eof() {
        let _guard = ENV_LOCK.lock().await;

        let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
        let prev_timeout = std::env::var(STDIO_IDLE_TIMEOUT_ENV).ok();
        let tmp_dir =
            std::env::temp_dir().join(format!("infra-server-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
        std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);
        std::env::set_var(STDIO_IDLE_TIMEOUT_ENV, "0");

        let server = McpServer::new().await.expect("server");
        let (mut client_writer, server_reader) = tokio::io::duplex(8 * 1024);
        let (server_writer, client_reader) = tokio::io::duplex(8 * 1024);
        let mut run = tokio::spawn(async move {
            server
                .run_with_io(
                    server_reader,
                    server_writer,
                    resolve_stdio_idle_timeout_ms(),
                )
                .await
        });

        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });
        client_writer
            .write_all(format!("{}\n", initialize).as_bytes())
            .await
            .expect("write initialize");

        let mut client_reader = BufReader::new(client_reader).lines();
        let response_line =
            tokio::time::timeout(Duration::from_millis(500), client_reader.next_line())
                .await
                .expect("initialize response before idle timeout")
                .expect("read initialize response")
                .expect("initialize response line");
        let response: serde_json::Value =
            serde_json::from_str(&response_line).expect("parse initialize response");
        assert_eq!(response.get("id"), Some(&serde_json::json!(1)));
        assert!(response.get("result").is_some());

        assert!(
            tokio::time::timeout(Duration::from_millis(125), &mut run)
                .await
                .is_err(),
            "server should remain alive while the leaked pipe stays open when idle timeout is disabled"
        );

        drop(client_writer);
        let run_result = tokio::time::timeout(Duration::from_millis(500), run)
            .await
            .expect("server exits after eof")
            .expect("join run loop");
        assert!(run_result.is_ok());

        restore_env("MCP_PROFILES_DIR", prev_profiles);
        restore_env(STDIO_IDLE_TIMEOUT_ENV, prev_timeout);
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[tokio::test]
    async fn initialize_and_mcp_native_surfaces_are_wired() {
        let _guard = ENV_LOCK.lock().await;

        let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
        let tmp_dir =
            std::env::temp_dir().join(format!("infra-server-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
        std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);

        let server = McpServer::new().await.expect("server");

        let init = server.handle_initialize().await;
        assert_eq!(
            init.get("serverInfo")
                .and_then(|v| v.get("version"))
                .and_then(|v| v.as_str()),
            Some(SERVER_VERSION)
        );
        assert_eq!(
            init.get("capabilities")
                .and_then(|v| v.get("resources"))
                .and_then(|v| v.get("read"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            init.get("capabilities")
                .and_then(|v| v.get("prompts"))
                .and_then(|v| v.get("get"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );

        let resources = server.handle_resources_list().await;
        let resource_uris = resources
            .get("resources")
            .and_then(|v| v.as_array())
            .expect("resource catalog");
        assert!(resource_uris.iter().any(|entry| {
            entry.get("uri").and_then(|v| v.as_str()) == Some("infra://workspace/store")
        }));

        let store_resource = server
            .handle_resources_read("infra://workspace/store")
            .await
            .expect("workspace/store resource");
        let store_text = store_resource
            .get("contents")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(store_text.contains("\"primary_store\""));
        assert!(store_text.contains("\"sqlite\""));

        let prompts = server.handle_prompts_list().await;
        let prompt_names = prompts
            .get("prompts")
            .and_then(|v| v.as_array())
            .expect("prompt catalog");
        assert!(prompt_names
            .iter()
            .any(|entry| { entry.get("name").and_then(|v| v.as_str()) == Some("deploy_service") }));

        let prompt = server
            .handle_prompts_get(
                "deploy_service",
                json!({ "target": "staging", "capability": "gitops.release" }),
            )
            .await
            .expect("deploy prompt");
        let prompt_text = prompt
            .get("messages")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.get("content"))
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(prompt_text.contains("staging"));
        assert!(prompt_text.contains("gitops.release"));

        restore_env("MCP_PROFILES_DIR", prev_profiles);
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[tokio::test]
    async fn tools_call_returns_structured_content_envelope() {
        let _guard = ENV_LOCK.lock().await;

        let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
        let tmp_dir =
            std::env::temp_dir().join(format!("infra-server-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
        std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);

        let server = McpServer::new().await.expect("server");
        let result = server
            .handle_tools_call("help", serde_json::json!({}))
            .await
            .expect("help call");
        let structured = result
            .get("structuredContent")
            .cloned()
            .expect("structured content");
        assert!(structured.get("tool").is_some());
        let text = result
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let parsed_text: serde_json::Value =
            serde_json::from_str(text).expect("json text envelope");
        assert_eq!(structured, parsed_text);

        restore_env("MCP_PROFILES_DIR", prev_profiles);
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[tokio::test]
    async fn tools_list_defaults_to_core_tier() {
        let _guard = ENV_LOCK.lock().await;

        let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
        let prev_tier = std::env::var("INFRA_TOOL_TIER").ok();
        let tmp_dir =
            std::env::temp_dir().join(format!("infra-server-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
        std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);
        std::env::remove_var("INFRA_TOOL_TIER");

        let server = McpServer::new().await.expect("server");
        let listed = server.handle_tools_list().await;
        let names: Vec<String> = listed
            .get("tools")
            .and_then(|v| v.as_array())
            .expect("tools array")
            .iter()
            .filter_map(|entry| {
                entry
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();

        assert!(names.contains(&"mcp_capability".to_string()));
        assert!(names.contains(&"mcp_operation".to_string()));
        assert!(names.contains(&"mcp_receipt".to_string()));
        assert!(names.contains(&"mcp_policy".to_string()));
        assert!(names.contains(&"mcp_profile".to_string()));
        assert!(names.contains(&"mcp_target".to_string()));
        assert!(!names.contains(&"mcp_project".to_string()));
        assert!(!names.contains(&"mcp_jobs".to_string()));
        assert!(!names.contains(&"mcp_artifacts".to_string()));
        assert!(!names.contains(&"mcp_workspace".to_string()));
        assert!(!names.contains(&"mcp_runbook".to_string()));
        let capability_tool = listed
            .get("tools")
            .and_then(|v| v.as_array())
            .and_then(|arr| {
                arr.iter().find(|entry| {
                    entry.get("name").and_then(|v| v.as_str()) == Some("mcp_capability")
                })
            })
            .expect("capability tool");
        let capability_actions: Vec<String> = capability_tool
            .get("inputSchema")
            .and_then(|v| v.get("properties"))
            .and_then(|v| v.get("action"))
            .and_then(|v| v.get("enum"))
            .and_then(|v| v.as_array())
            .expect("capability action enum")
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        assert!(!capability_actions.contains(&"set".to_string()));
        assert!(!capability_actions.contains(&"delete".to_string()));

        for (tool_name, expected) in [
            ("mcp_receipt", vec!["list", "get"]),
            ("mcp_policy", vec!["resolve", "evaluate"]),
            ("mcp_profile", vec!["list", "get"]),
            ("mcp_target", vec!["list", "get", "resolve"]),
        ] {
            let entry = listed
                .get("tools")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter()
                        .find(|entry| entry.get("name").and_then(|v| v.as_str()) == Some(tool_name))
                })
                .unwrap_or_else(|| panic!("{} tool", tool_name));
            let actions: Vec<String> = entry
                .get("inputSchema")
                .and_then(|v| v.get("properties"))
                .and_then(|v| v.get("action"))
                .and_then(|v| v.get("enum"))
                .and_then(|v| v.as_array())
                .expect("action enum")
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            assert_eq!(actions, expected, "{}", tool_name);
        }

        restore_env("INFRA_TOOL_TIER", prev_tier);
        restore_env("MCP_PROFILES_DIR", prev_profiles);
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[tokio::test]
    async fn tools_list_expert_tier_restores_expanded_surface() {
        let _guard = ENV_LOCK.lock().await;

        let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
        let prev_tier = std::env::var("INFRA_TOOL_TIER").ok();
        let tmp_dir =
            std::env::temp_dir().join(format!("infra-server-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
        std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);
        std::env::set_var("INFRA_TOOL_TIER", "expert");

        let server = McpServer::new().await.expect("server");
        let listed = server.handle_tools_list().await;
        let names: Vec<String> = listed
            .get("tools")
            .and_then(|v| v.as_array())
            .expect("tools array")
            .iter()
            .filter_map(|entry| {
                entry
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();

        assert!(names.contains(&"mcp_workspace".to_string()));
        assert!(names.contains(&"mcp_runbook".to_string()));
        assert!(names.contains(&"mcp_receipt".to_string()));
        assert!(names.contains(&"mcp_policy".to_string()));
        assert!(names.contains(&"mcp_profile".to_string()));
        assert!(names.contains(&"mcp_target".to_string()));

        restore_env("INFRA_TOOL_TIER", prev_tier);
        restore_env("MCP_PROFILES_DIR", prev_profiles);
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[tokio::test]
    async fn help_defaults_to_core_overview_surface() {
        let _guard = ENV_LOCK.lock().await;

        let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
        let prev_tier = std::env::var("INFRA_TOOL_TIER").ok();
        let tmp_dir =
            std::env::temp_dir().join(format!("infra-server-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
        std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);
        std::env::remove_var("INFRA_TOOL_TIER");

        let server = McpServer::new().await.expect("server");
        let help_result = server
            .handle_tools_call("help", serde_json::json!({}))
            .await
            .expect("help");
        let payload = help_result
            .get("structuredContent")
            .and_then(|v| v.get("result"))
            .cloned()
            .expect("help payload");
        let tool_names: Vec<String> = payload
            .get("tools")
            .and_then(|v| v.as_array())
            .expect("help tool list")
            .iter()
            .filter_map(|entry| {
                entry
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();

        assert!(tool_names.contains(&"mcp_capability".to_string()));
        assert!(tool_names.contains(&"mcp_operation".to_string()));
        assert!(tool_names.contains(&"mcp_receipt".to_string()));
        assert!(tool_names.contains(&"mcp_policy".to_string()));
        assert!(tool_names.contains(&"mcp_profile".to_string()));
        assert!(tool_names.contains(&"mcp_target".to_string()));
        assert!(!tool_names.contains(&"mcp_project".to_string()));
        assert!(!tool_names.contains(&"mcp_workspace".to_string()));

        restore_env("INFRA_TOOL_TIER", prev_tier);
        restore_env("MCP_PROFILES_DIR", prev_profiles);
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[tokio::test]
    async fn help_capability_in_core_mode_hides_write_actions() {
        let _guard = ENV_LOCK.lock().await;

        let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
        let prev_tier = std::env::var("INFRA_TOOL_TIER").ok();
        let tmp_dir =
            std::env::temp_dir().join(format!("infra-server-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
        std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);
        std::env::remove_var("INFRA_TOOL_TIER");

        let server = McpServer::new().await.expect("server");
        let help_result = server
            .handle_tools_call("help", serde_json::json!({ "tool": "mcp_capability" }))
            .await
            .expect("capability help");
        let payload = help_result
            .get("structuredContent")
            .and_then(|v| v.get("result"))
            .cloned()
            .expect("help payload");
        let actions: Vec<String> = payload
            .get("actions")
            .and_then(|v| v.as_array())
            .expect("actions")
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();

        assert!(!actions.contains(&"set".to_string()));
        assert!(!actions.contains(&"delete".to_string()));
        assert_eq!(
            payload.get("usage").and_then(|v| v.as_str()),
            Some("list/get/resolve/families/suggest/graph/stats")
        );

        restore_env("INFRA_TOOL_TIER", prev_tier);
        restore_env("MCP_PROFILES_DIR", prev_profiles);
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[tokio::test]
    async fn help_expert_overview_hides_legacy_discovery_tools() {
        let _guard = ENV_LOCK.lock().await;

        let prev_profiles = std::env::var("MCP_PROFILES_DIR").ok();
        let prev_tier = std::env::var("INFRA_TOOL_TIER").ok();
        let tmp_dir =
            std::env::temp_dir().join(format!("infra-server-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
        std::env::set_var("MCP_PROFILES_DIR", &tmp_dir);
        std::env::set_var("INFRA_TOOL_TIER", "expert");

        let server = McpServer::new().await.expect("server");
        let help_result = server
            .handle_tools_call("help", serde_json::json!({}))
            .await
            .expect("help");
        let payload = help_result
            .get("structuredContent")
            .and_then(|v| v.get("result"))
            .cloned()
            .expect("help payload");
        let tool_names: Vec<String> = payload
            .get("tools")
            .and_then(|v| v.as_array())
            .expect("help tool list")
            .iter()
            .filter_map(|entry| {
                entry
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();

        assert!(!tool_names.contains(&"mcp_intent".to_string()));
        assert!(!tool_names.contains(&"mcp_pipeline".to_string()));

        restore_env("INFRA_TOOL_TIER", prev_tier);
        restore_env("MCP_PROFILES_DIR", prev_profiles);
        std::fs::remove_dir_all(&tmp_dir).ok();
    }
}
