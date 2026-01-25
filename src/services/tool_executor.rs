use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

use crate::errors::ToolError;
use crate::services::alias::AliasService;
use crate::services::audit::AuditService;
use crate::services::logger::Logger;
use crate::services::preset::PresetService;
use crate::services::state::StateService;
use crate::utils::artifacts::{
    build_tool_call_file_ref, resolve_context_root, write_text_artifact,
};
use crate::utils::merge::merge_deep;
use crate::utils::output::apply_output_transform;
use crate::utils::redact::{is_sensitive_key, redact_object, redact_text};
use crate::utils::suggest::suggest;
use crate::utils::text::{truncate_utf8_prefix, truncate_utf8_suffix};

use serde_json::Value;

#[async_trait]
pub trait ToolHandler: Send + Sync {
    async fn handle(&self, args: Value) -> Result<Value, ToolError>;
}

#[derive(Clone)]
pub struct ToolExecutor {
    logger: Logger,
    state_service: Arc<StateService>,
    alias_service: Option<Arc<AliasService>>,
    preset_service: Option<Arc<PresetService>>,
    audit_service: Option<Arc<AuditService>>,
    handlers: Arc<HashMap<String, Arc<dyn ToolHandler>>>,
    alias_map: HashMap<String, String>,
}

#[derive(Clone)]
pub(crate) struct ToolCallMeta {
    pub started_at: i64,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub invoked_as: Option<String>,
    pub preset_name: Option<String>,
}

impl ToolExecutor {
    pub fn new(
        logger: Logger,
        state_service: Arc<StateService>,
        alias_service: Option<Arc<AliasService>>,
        preset_service: Option<Arc<PresetService>>,
        audit_service: Option<Arc<AuditService>>,
        handlers: HashMap<String, Arc<dyn ToolHandler>>,
        alias_map: HashMap<String, String>,
    ) -> Self {
        Self {
            logger: logger.child("executor"),
            state_service,
            alias_service,
            preset_service,
            audit_service,
            handlers: Arc::new(handlers),
            alias_map,
        }
    }

    async fn resolve_alias(&self, tool: &str) -> (String, Option<Value>) {
        if self.handlers.contains_key(tool) {
            return (tool.to_string(), None);
        }
        if let Some(mapped) = self.alias_map.get(tool) {
            return (
                mapped.clone(),
                Some(serde_json::json!({"name": tool, "tool": mapped})),
            );
        }
        if let Some(service) = &self.alias_service {
            if let Some(alias) = service.resolve_alias(tool) {
                let target = alias
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| tool.to_string());
                let mapped = if self.handlers.contains_key(&target) {
                    target.clone()
                } else {
                    self.alias_map
                        .get(&target)
                        .cloned()
                        .unwrap_or(target.clone())
                };
                let mut alias_value = alias.clone();
                if let Value::Object(map) = &mut alias_value {
                    map.insert("name".to_string(), Value::String(tool.to_string()));
                    map.insert("tool".to_string(), Value::String(mapped.clone()));
                }
                return (mapped, Some(alias_value));
            }
        }
        (tool.to_string(), None)
    }

    fn normalize_store_target(
        &self,
        store_as: Option<&Value>,
        store_scope: Option<&Value>,
    ) -> Option<(String, String)> {
        if let Some(Value::String(key)) = store_as {
            let scope = store_scope
                .and_then(|v| v.as_str())
                .unwrap_or("session")
                .to_string();
            return Some((key.to_string(), scope));
        }
        if let Some(Value::Object(obj)) = store_as {
            if let Some(key) = obj.get("key").and_then(|v| v.as_str()) {
                let scope = obj
                    .get("scope")
                    .and_then(|v| v.as_str())
                    .or_else(|| store_scope.and_then(|v| v.as_str()))
                    .unwrap_or("session")
                    .to_string();
                return Some((key.to_string(), scope));
            }
        }
        None
    }

    fn normalize_preset_data(&self, preset: Option<Value>) -> Option<Value> {
        let preset = preset?;
        if let Some(data) = preset.get("data") {
            if data.is_object() {
                return Some(data.clone());
            }
        }
        if let Some(obj) = preset.as_object() {
            let mut out = obj.clone();
            out.remove("created_at");
            out.remove("updated_at");
            out.remove("description");
            return Some(Value::Object(out));
        }
        None
    }

    fn normalize_alias_args(&self, alias: Option<&Value>) -> Option<Value> {
        let alias = alias?;
        alias.get("args").cloned().filter(|v| v.is_object())
    }

    fn merge_args(
        &self,
        preset: Option<&Value>,
        alias_args: Option<&Value>,
        args: &Value,
    ) -> Value {
        let mut merged = Value::Object(Default::default());
        if let Some(preset) = preset {
            merged = merge_deep(&merged, preset);
        }
        if let Some(alias_args) = alias_args {
            merged = merge_deep(&merged, alias_args);
        }
        merged = merge_deep(&merged, args);
        merged
    }

    fn strip_args_for_handler(&self, args: &Value) -> Value {
        let mut cleaned = args.clone();
        if let Value::Object(map) = &mut cleaned {
            map.remove("output");
            map.remove("store_as");
            map.remove("store_scope");
            map.remove("preset");
            map.remove("preset_name");
        }
        cleaned
    }

    fn build_audit_args(&self, args: &Value) -> Value {
        let mut cleaned = self.strip_args_for_handler(args);
        if let Value::Object(map) = &mut cleaned {
            if let Some(Value::String(base64)) = map.get("body_base64") {
                map.insert(
                    "body_base64".to_string(),
                    Value::String(format!("[base64:{}]", base64.len())),
                );
            }
        }
        redact_object(&cleaned, 2048, None)
    }

    fn summarize_result(&self, result: &Value) -> Value {
        if result.is_null() {
            return serde_json::json!({"type": "null"});
        }
        if let Some(arr) = result.as_array() {
            return serde_json::json!({"type": "array", "length": arr.len()});
        }
        if let Some(obj) = result.as_object() {
            let keys: Vec<String> = obj.keys().take(10).cloned().collect();
            return serde_json::json!({"type": "object", "keys": keys, "key_count": obj.len()});
        }
        serde_json::json!({"type": value_type_name(result), "value": result})
    }

    fn spill_large_values(
        value: &Value,
        path_segments: &[String],
        ctx: &SpillContext,
        state: &mut SpillState,
    ) -> Result<Value, ToolError> {
        if value.is_null() {
            return Ok(Value::Null);
        }
        if let Some(text) = value.as_str() {
            let bytes = text.len();
            if bytes <= ctx.max_inline_bytes {
                return Ok(Value::String(redact_text(
                    text,
                    usize::MAX,
                    ctx.extra_secrets.as_deref(),
                )));
            }
            let has_sensitive = path_segments.iter().any(|s| is_sensitive_key(s));
            let preview_limit = (ctx.max_inline_bytes / 4).clamp(128, 2048);
            let preview = truncate_utf8_prefix(text, preview_limit);
            let tail = if text.len() > preview_limit {
                truncate_utf8_suffix(text, preview_limit)
            } else {
                String::new()
            };
            let capped = truncate_utf8_prefix(text, ctx.max_capture_bytes);
            let sha256 = compute_sha256(capped.as_bytes());
            let capture_truncated = bytes > ctx.max_capture_bytes;

            let mut artifact = Value::Null;
            if !has_sensitive && ctx.context_root.is_some() && state.spilled < ctx.max_spills {
                let filename = resolve_spill_filename(path_segments, state);
                let reference = build_tool_call_file_ref(
                    ctx.trace_id.as_deref(),
                    ctx.span_id.as_deref(),
                    &filename,
                )?;
                let written =
                    write_text_artifact(ctx.context_root.as_ref().unwrap(), &reference, &capped)?;
                state.spilled += 1;
                artifact = serde_json::json!({
                    "uri": written.uri,
                    "rel": written.rel,
                    "bytes": written.bytes,
                    "truncated": capture_truncated,
                });
            }
            return Ok(serde_json::json!({
                "truncated": true,
                "bytes": bytes,
                "sha256": sha256,
                "artifact": artifact,
                "preview": redact_text(&preview, usize::MAX, ctx.extra_secrets.as_deref()),
                "tail": redact_text(&tail, usize::MAX, ctx.extra_secrets.as_deref()),
            }));
        }
        if let Some(arr) = value.as_array() {
            let mut out = Vec::new();
            for (idx, item) in arr.iter().enumerate() {
                let mut next_path = path_segments.to_vec();
                next_path.push(idx.to_string());
                out.push(Self::spill_large_values(item, &next_path, ctx, state)?);
            }
            return Ok(Value::Array(out));
        }
        if let Some(obj) = value.as_object() {
            let mut out = serde_json::Map::new();
            for (key, val) in obj {
                if key.ends_with("_buffer") {
                    if let Some(existing_ref) = obj.get(&key.replace("_buffer", "_ref")) {
                        let bytes = buffer_length(val);
                        let sha256 = compute_sha256(buffer_bytes(val).as_slice());
                        out.insert(
                            key.clone(),
                            serde_json::json!({
                                "truncated": true,
                                "bytes": bytes,
                                "sha256": sha256,
                                "artifact": existing_ref,
                                "preview": "",
                                "tail": "",
                            }),
                        );
                        continue;
                    }
                }
                let mut next_path = path_segments.to_vec();
                next_path.push(key.clone());
                let spilled = Self::spill_large_values(val, &next_path, ctx, state)?;
                out.insert(key.clone(), spilled);
            }
            return Ok(Value::Object(out));
        }
        Ok(value.clone())
    }

    pub(crate) async fn wrap_result(
        &self,
        tool: &str,
        args: &Value,
        result: &Value,
        meta: ToolCallMeta,
    ) -> Result<Value, ToolError> {
        let ToolCallMeta {
            started_at,
            trace_id,
            span_id,
            parent_span_id,
            invoked_as,
            preset_name,
        } = meta;
        let output = args.get("output");
        let store = self.normalize_store_target(args.get("store_as"), args.get("store_scope"));
        let shaped = apply_output_transform(result, output)?;

        let context_root = resolve_context_root();
        let max_inline_bytes = env_u64("INFRA_MAX_INLINE_BYTES", 16 * 1024) as usize;
        let max_capture_bytes = env_u64("INFRA_MAX_CAPTURE_BYTES", 256 * 1024) as usize;
        let max_spills = env_u64("INFRA_MAX_SPILLS", 20) as usize;

        let extra_secrets = collect_secret_values(args.get("env"));
        let ctx = SpillContext {
            context_root,
            trace_id: Some(trace_id.clone()),
            span_id: Some(span_id.clone()),
            max_inline_bytes,
            max_capture_bytes,
            max_spills,
            extra_secrets,
        };
        let mut state = SpillState {
            used_names: HashMap::new(),
            spilled: 0,
        };
        let spilled = Self::spill_large_values(&shaped, &[], &ctx, &mut state)?;

        if let Some((key, scope)) = store {
            let _ = self.state_service.set(&key, spilled.clone(), Some(&scope));
        }

        let meta = serde_json::json!({
            "tool": tool,
            "action": args.get("action").cloned().unwrap_or(Value::Null),
            "trace_id": trace_id,
            "span_id": span_id,
            "parent_span_id": parent_span_id,
            "duration_ms": chrono::Utc::now().timestamp_millis() - started_at,
            "stored_as": args.get("store_as").cloned().unwrap_or(Value::Null),
            "invoked_as": invoked_as,
            "preset": preset_name,
        });

        Ok(serde_json::json!({
            "ok": true,
            "result": spilled,
            "meta": meta,
        }))
    }

    pub async fn execute(&self, tool: &str, args: Value) -> Result<Value, ToolError> {
        let started_at = chrono::Utc::now().timestamp_millis();
        let (resolved_tool, alias) = self.resolve_alias(tool).await;
        let handler = self.handlers.get(&resolved_tool);
        if handler.is_none() {
            let candidates: Vec<String> = self
                .handlers
                .keys()
                .cloned()
                .chain(self.alias_map.keys().cloned())
                .collect();
            let suggestions = suggest(tool, &candidates, 6);
            let hint = if suggestions.is_empty() {
                "Call help() to list available tools".to_string()
            } else {
                format!(
                    "Did you mean: {} (or call help() for the full list)",
                    suggestions.join(", ")
                )
            };
            return Err(
                ToolError::invalid_params(format!("Unknown tool: {}", tool)).with_hint(hint)
            );
        }
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

        let mut preset_name = args
            .get("preset")
            .or_else(|| args.get("preset_name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if preset_name.is_none() {
            if let Some(alias_value) = alias.as_ref() {
                if let Some(alias_preset) = alias_value.get("preset").and_then(|v| v.as_str()) {
                    preset_name = Some(alias_preset.to_string());
                }
            }
        }

        let preset_data = if let Some(name) = preset_name.as_ref() {
            self.preset_service
                .as_ref()
                .and_then(|service| service.resolve_preset(name))
        } else {
            None
        };
        let normalized_preset = self.normalize_preset_data(preset_data);
        let alias_args = self.normalize_alias_args(alias.as_ref());
        let merged_args = self.merge_args(normalized_preset.as_ref(), alias_args.as_ref(), &args);

        let mut merged_args = merged_args;
        if let Value::Object(map) = &mut merged_args {
            map.insert("trace_id".to_string(), Value::String(trace_id.clone()));
            map.insert("span_id".to_string(), Value::String(span_id.clone()));
            if let Some(parent) = parent_span_id.as_ref() {
                map.insert("parent_span_id".to_string(), Value::String(parent.clone()));
            }
        }

        let cleaned_args = self.strip_args_for_handler(&merged_args);
        let invoked_as = alias
            .as_ref()
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        self.logger
            .debug(resolved_tool.as_str(), merged_args.get("action"));

        let result = handler.unwrap().handle(cleaned_args).await?;
        let payload = self
            .wrap_result(
                &resolved_tool,
                &merged_args,
                &result,
                ToolCallMeta {
                    started_at,
                    trace_id: trace_id.clone(),
                    span_id: span_id.clone(),
                    parent_span_id: parent_span_id.clone(),
                    invoked_as: invoked_as.clone(),
                    preset_name: preset_name.clone(),
                },
            )
            .await?;

        if let Some(audit) = &self.audit_service {
            audit.append(&serde_json::json!({
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "status": "ok",
                "tool": resolved_tool,
                "action": merged_args.get("action"),
                "trace_id": trace_id,
                "span_id": span_id,
                "parent_span_id": parent_span_id,
                "invoked_as": invoked_as,
                "input": self.build_audit_args(&merged_args),
                "result_summary": self.summarize_result(payload.get("result").unwrap_or(&Value::Null)),
                "duration_ms": chrono::Utc::now().timestamp_millis() - started_at,
            }));
        }

        Ok(payload)
    }
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

struct SpillContext {
    context_root: Option<std::path::PathBuf>,
    trace_id: Option<String>,
    span_id: Option<String>,
    max_inline_bytes: usize,
    max_capture_bytes: usize,
    max_spills: usize,
    extra_secrets: Option<Vec<String>>,
}

struct SpillState {
    used_names: HashMap<String, usize>,
    spilled: usize,
}

fn resolve_spill_filename(path_segments: &[String], state: &mut SpillState) -> String {
    let mut normalized: Vec<String> = path_segments
        .iter()
        .filter(|segment| !segment.trim().is_empty())
        .rev()
        .take(6)
        .map(|s| safe_filename_segment(s))
        .collect();
    normalized.reverse();
    let base = if normalized.is_empty() {
        "value".to_string()
    } else {
        normalized.join("__")
    };
    let safe_base = if base.len() > 120 {
        base[..120].to_string()
    } else {
        base
    };
    let candidate = format!("{}.txt", safe_base);
    let count = state.used_names.entry(candidate.clone()).or_insert(0);
    *count += 1;
    if *count == 1 {
        return candidate;
    }
    let name = candidate.trim_end_matches(".txt");
    format!("{}--{}.txt", name, count)
}

fn safe_filename_segment(value: &str) -> String {
    let base = value.trim();
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('_');
    if cleaned.is_empty() {
        "value".to_string()
    } else {
        cleaned.chars().take(64).collect()
    }
}

fn compute_sha256(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hex::encode(hasher.finalize())
}

fn buffer_length(value: &Value) -> usize {
    if let Some(text) = value.as_str() {
        return text.len();
    }
    if let Some(arr) = value.as_array() {
        return arr.len();
    }
    0
}

fn buffer_bytes(value: &Value) -> Vec<u8> {
    if let Some(text) = value.as_str() {
        return text.as_bytes().to_vec();
    }
    if let Some(arr) = value.as_array() {
        return arr
            .iter()
            .filter_map(|v| v.as_u64().map(|b| b as u8))
            .collect();
    }
    Vec::new()
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn collect_secret_values(value: Option<&Value>) -> Option<Vec<String>> {
    let Some(Value::Object(map)) = value else {
        return None;
    };
    let mut out = Vec::new();
    for v in map.values() {
        if let Some(text) = v.as_str() {
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
