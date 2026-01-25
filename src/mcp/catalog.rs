use crate::errors::{ErrorCode, McpError};
use crate::mcp::aliases::builtin_tool_aliases;
use crate::utils::feature_flags::is_unsafe_local_enabled;
use crate::utils::suggest::suggest;
use jsonschema::JSONSchema;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

static TOOL_CATALOG: Lazy<Vec<ToolDef>> = Lazy::new(|| {
    let raw = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tool_catalog.json"));
    serde_json::from_str(raw).expect("tool_catalog.json must be valid JSON")
});

static TOOL_MAP: Lazy<HashMap<String, ToolDef>> = Lazy::new(|| {
    TOOL_CATALOG
        .iter()
        .cloned()
        .map(|tool| (tool.name.clone(), tool))
        .collect()
});

static TOOL_VALIDATORS: Lazy<HashMap<String, JSONSchema>> = Lazy::new(|| {
    let mut map = HashMap::new();
    for tool in TOOL_CATALOG.iter() {
        if let Ok(schema) = JSONSchema::compile(&tool.input_schema) {
            map.insert(tool.name.clone(), schema);
        }
    }
    map
});

const TOOL_SEMANTIC_FIELDS: &[&str] = &[
    "output",
    "store_as",
    "store_scope",
    "trace_id",
    "span_id",
    "parent_span_id",
    "preset",
    "preset_name",
    "response_mode",
];

pub fn tool_catalog() -> &'static Vec<ToolDef> {
    &TOOL_CATALOG
}

pub fn tool_by_name(name: &str) -> Option<&'static ToolDef> {
    TOOL_MAP.get(name)
}

pub fn validate_tool_args(tool_name: &str, args: &Value) -> Result<(), McpError> {
    let Some(tool) = tool_by_name(tool_name) else {
        return Ok(());
    };
    let schema = TOOL_VALIDATORS.get(tool_name);
    if schema.is_none() {
        return Ok(());
    }
    let schema = schema.unwrap();
    if let Err(errors) = schema.validate(args) {
        let message = format_schema_errors(tool_name, args, errors, &tool.input_schema);
        return Err(McpError::new(ErrorCode::InvalidParams, message));
    }
    Ok(())
}

fn format_schema_errors(
    tool_name: &str,
    args: &Value,
    errors: jsonschema::ErrorIterator,
    schema: &Value,
) -> String {
    let action = args.get("action").and_then(|v| v.as_str());
    let header = if let Some(action) = action {
        format!("Invalid arguments for {}:{}", tool_name, action)
    } else {
        format!("Invalid arguments for {}", tool_name)
    };
    let mut rendered = Vec::new();
    let mut did_you_means = Vec::new();
    let mut suggested_action = None;

    for err in errors.take(10) {
        let instance_path = if err.instance_path.to_string().is_empty() {
            "(root)".to_string()
        } else {
            err.instance_path.to_string()
        };
        match &err.kind {
            jsonschema::error::ValidationErrorKind::AdditionalProperties { unexpected } => {
                if unexpected.is_empty() {
                    rendered.push(format!("{}: unknown field", instance_path));
                }
                for unknown in unexpected {
                    rendered.push(format!("{}: unknown field '{}'", instance_path, unknown));
                    if let Some(parent) = schema_parent_at(schema, err.schema_path.to_string()) {
                        let props: Vec<String> = parent
                            .get("properties")
                            .and_then(|v| v.as_object())
                            .map(|map| map.keys().cloned().collect())
                            .unwrap_or_default();
                        let suggestions = suggest(unknown, &props, 3);
                        if !suggestions.is_empty() {
                            did_you_means.push(format!(
                                "field '{}': {}",
                                unknown,
                                suggestions.join(", ")
                            ));
                        }
                    }
                }
            }
            jsonschema::error::ValidationErrorKind::Enum { options } => {
                let allowed_list: Vec<String> = options
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .map(|v| {
                                v.as_str()
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| v.to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if allowed_list.is_empty() {
                    rendered.push(format!("{}: invalid value", instance_path));
                } else {
                    rendered.push(format!(
                        "{}: expected one of {}",
                        instance_path,
                        allowed_list
                            .iter()
                            .take(12)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    let received = schema_node_at(args, &err.instance_path.to_string());
                    let received_str = received.as_str().unwrap_or("");
                    let suggestions = suggest(received_str, &allowed_list, 3);
                    if !suggestions.is_empty() {
                        did_you_means.push(format!(
                            "{}: {}",
                            instance_path,
                            suggestions.join(", ")
                        ));
                        if err.instance_path.to_string() == "/action" {
                            suggested_action = suggestions.first().cloned();
                        }
                    }
                }
            }
            jsonschema::error::ValidationErrorKind::Required { property } => {
                let prop = property
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| property.to_string());
                rendered.push(format!(
                    "{}: missing required field '{}'",
                    instance_path, prop
                ));
            }
            jsonschema::error::ValidationErrorKind::Type { kind } => {
                rendered.push(format!(
                    "{}: expected {}",
                    instance_path,
                    format_type_kind(kind)
                ));
            }
            _ => {
                rendered.push(format!("{}: {}", instance_path, err));
            }
        }
    }

    let mut lines = vec![header];
    lines.extend(rendered.iter().map(|line| format!("- {}", line)));
    if !did_you_means.is_empty() {
        lines.push(format!(
            "Did you mean: {}",
            did_you_means
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if let Some(action) = suggested_action.or_else(|| action.map(|s| s.to_string())) {
        lines.push(format!(
            "Hint: help({{ tool: '{}', action: '{}' }})",
            tool_name, action
        ));
    } else {
        lines.push(format!("Hint: help({{ tool: '{}' }})", tool_name));
    }
    lines.join("\n")
}

fn format_type_kind(kind: &jsonschema::error::TypeKind) -> String {
    match kind {
        jsonschema::error::TypeKind::Single(primitive) => primitive.to_string(),
        jsonschema::error::TypeKind::Multiple(types) => {
            let list: Vec<String> = (*types).into_iter().map(|t| t.to_string()).collect();
            if list.is_empty() {
                "unknown".to_string()
            } else {
                list.join(" | ")
            }
        }
    }
}

fn schema_parent_at(schema: &Value, schema_path: String) -> Option<Value> {
    let mut current = schema;
    for segment in schema_path.split('/') {
        if segment.is_empty() {
            continue;
        }
        if let Some(obj) = current.as_object() {
            current = obj.get(segment)?;
        } else if let Some(arr) = current.as_array() {
            let idx = segment.parse::<usize>().ok()?;
            current = arr.get(idx)?;
        }
    }
    Some(current.clone())
}

fn schema_node_at(root: &Value, instance_path: &str) -> Value {
    if instance_path.is_empty() {
        return root.clone();
    }
    let mut current = root;
    for segment in instance_path.trim_start_matches('/').split('/') {
        if segment.is_empty() {
            continue;
        }
        if let Some(obj) = current.as_object() {
            current = obj.get(segment).unwrap_or(&Value::Null);
        } else if let Some(arr) = current.as_array() {
            let idx = segment.parse::<usize>().unwrap_or(0);
            current = arr.get(idx).unwrap_or(&Value::Null);
        }
    }
    current.clone()
}

pub fn normalize_json_schema_for_openai(schema: &Value) -> Value {
    match schema {
        Value::Null => Value::Null,
        Value::Array(items) => {
            Value::Array(items.iter().map(normalize_json_schema_for_openai).collect())
        }
        Value::Object(map) => {
            let mut out = map.clone();
            if let Some(props) = out.get("properties").and_then(|v| v.as_object()) {
                let mut normalized = serde_json::Map::new();
                for (key, value) in props {
                    normalized.insert(key.clone(), normalize_json_schema_for_openai(value));
                }
                out.insert("properties".to_string(), Value::Object(normalized));
            }
            if let Some(items) = out.get("items") {
                out.insert("items".to_string(), normalize_json_schema_for_openai(items));
            }
            if let Some(additional) = out.get("additionalProperties") {
                if additional.is_object() {
                    out.insert(
                        "additionalProperties".to_string(),
                        normalize_json_schema_for_openai(additional),
                    );
                }
            }
            for keyword in ["anyOf", "oneOf", "allOf"] {
                if let Some(arr) = out.get(keyword).and_then(|v| v.as_array()) {
                    out.insert(
                        keyword.to_string(),
                        Value::Array(arr.iter().map(normalize_json_schema_for_openai).collect()),
                    );
                }
            }
            if let Some(types) = out.get("type").and_then(|v| v.as_array()) {
                let mut shared = out.clone();
                shared.remove("type");
                let items = shared.get("items").cloned();
                shared.remove("items");
                let any_of = types
                    .iter()
                    .filter_map(|t| t.as_str())
                    .map(|t| {
                        if t == "array" {
                            serde_json::json!({"type": "array", "items": items.clone().unwrap_or(Value::Object(Default::default()))})
                        } else {
                            serde_json::json!({"type": t})
                        }
                    })
                    .collect();
                shared.insert("anyOf".to_string(), Value::Array(any_of));
                return Value::Object(shared);
            }
            if out.get("type").and_then(|v| v.as_str()) == Some("array")
                && !out.contains_key("items")
            {
                out.insert("items".to_string(), Value::Object(Default::default()));
            }
            Value::Object(out)
        }
        _ => schema.clone(),
    }
}

pub fn strip_tool_semantic_fields(schema: &Value) -> Value {
    if let Some(obj) = schema.as_object() {
        let mut out = obj.clone();
        if let Some(props) = out.get_mut("properties").and_then(|v| v.as_object_mut()) {
            for key in TOOL_SEMANTIC_FIELDS.iter() {
                props.remove(*key);
            }
        }
        if let Some(required) = out.get_mut("required").and_then(|v| v.as_array_mut()) {
            required.retain(|v| {
                v.as_str()
                    .map(|s| !TOOL_SEMANTIC_FIELDS.contains(&s))
                    .unwrap_or(true)
            });
        }
        return Value::Object(out);
    }
    schema.clone()
}

pub fn list_tools_for_openai(tool_tier: &str, core_tools: &HashSet<String>) -> Vec<ToolDef> {
    let mut tools = Vec::new();
    let unsafe_local = is_unsafe_local_enabled();
    for tool in TOOL_CATALOG.iter() {
        if tool_tier == "core" && !core_tools.contains(&tool.name) {
            continue;
        }
        if tool.name == "mcp_local" && !unsafe_local {
            continue;
        }
        let normalized = normalize_json_schema_for_openai(&tool.input_schema);
        let minimized = strip_tool_semantic_fields(&normalized);
        tools.push(ToolDef {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: minimized,
        });
    }

    if tool_tier != "core" {
        let mut names: HashSet<String> = tools.iter().map(|tool| tool.name.clone()).collect();
        for (alias, target) in builtin_tool_aliases().iter() {
            if !unsafe_local && *alias == "local" {
                continue;
            }
            if names.contains(*alias) {
                continue;
            }
            let Some(target_tool) = tools.iter().find(|tool| tool.name == *target) else {
                continue;
            };
            tools.push(ToolDef {
                name: (*alias).to_string(),
                description: format!("Alias for {}.", target),
                input_schema: target_tool.input_schema.clone(),
            });
            names.insert((*alias).to_string());
        }
    }

    tools
}
