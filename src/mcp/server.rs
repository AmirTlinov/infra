use crate::app::App;
use crate::errors::{ErrorCode, McpError, ToolError};
use crate::mcp::aliases::canonical_tool_name;
use crate::mcp::catalog::{list_tools_for_openai, tool_by_name, validate_tool_args};
use crate::mcp::envelope::{
    build_generic_envelope, build_local_exec_envelope, build_repo_exec_envelope,
    build_ssh_exec_envelope,
};
use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::mcp::{help, legend};
use crate::services::tool_executor::ToolCallMeta;
use crate::utils::arg_aliases::normalize_args_aliases;
use crate::utils::artifacts::{
    build_tool_call_context_ref, build_tool_call_file_ref, resolve_context_root,
    write_text_artifact,
};
use crate::utils::redact::redact_text;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

const PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "infra";
const SERVER_VERSION: &str = "7.0.1";

fn core_tool_names() -> HashSet<String> {
    HashSet::from([
        "help".to_string(),
        "legend".to_string(),
        "mcp_workspace".to_string(),
        "mcp_jobs".to_string(),
        "mcp_artifacts".to_string(),
        "mcp_project".to_string(),
    ])
}

fn resolve_tool_tier() -> String {
    let raw = std::env::var("INFRA_TOOL_TIER").unwrap_or_else(|_| "full".to_string());
    let normalized = raw.trim().to_lowercase();
    if normalized == "core" {
        "core".to_string()
    } else {
        "full".to_string()
    }
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
            "capabilities": {"tools": {"list": true, "call": true}},
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
        })
    }

    async fn handle_tools_list(&self) -> Value {
        let tier = resolve_tool_tier();
        let tools = list_tools_for_openai(&tier, &core_tool_names());
        serde_json::json!({ "tools": tools })
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
                        preset_name: None,
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
                        preset_name: None,
                    },
                )
                .await
                .map_err(|err| map_tool_error(name, &err))?
        } else if name == "mcp_runbook" || name == "runbook" {
            let result = self
                .app
                .runbook_manager
                .handle_action(args.clone())
                .await
                .map_err(|err| map_tool_error(name, &err))?;
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
                        preset_name: None,
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
            "content": [ { "type": "text", "text": serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string()) } ]
        }))
    }

    pub async fn run_stdio(&self) -> Result<(), ToolError> {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin).lines();
        let mut writer = BufWriter::new(stdout);

        while let Some(line) = reader
            .next_line()
            .await
            .map_err(|err| ToolError::internal(err.to_string()))?
        {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let parsed: Value = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(_) => {
                    let response = JsonRpcResponse::failure(
                        Value::Null,
                        ErrorCode::ParseError.as_i32(),
                        "Parse error".to_string(),
                    );
                    let payload = serde_json::to_string(&response).unwrap_or_default();
                    writer.write_all(payload.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    writer.flush().await?;
                    continue;
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
                    let payload = serde_json::to_string(&response).unwrap_or_default();
                    writer.write_all(payload.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    writer.flush().await?;
                    continue;
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
                _ => request.id.clone().map(|id| {
                    JsonRpcResponse::failure(
                        id,
                        ErrorCode::MethodNotFound.as_i32(),
                        "Method not found".to_string(),
                    )
                }),
            };

            if let Some(response) = response {
                let payload = serde_json::to_string(&response).unwrap_or_default();
                writer.write_all(payload.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
            }
        }

        Ok(())
    }
}

pub async fn run_stdio() -> Result<(), ToolError> {
    let server = McpServer::new().await?;
    server.run_stdio().await
}
