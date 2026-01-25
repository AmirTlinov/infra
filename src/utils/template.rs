use crate::errors::ToolError;
use crate::utils::data_path::get_path_value;
use serde_json::Value;

fn normalize_expression(expression: &str) -> (String, bool) {
    let trimmed = expression.trim();
    if trimmed.is_empty() {
        return (String::new(), false);
    }
    if let Some(stripped) = trimmed.strip_prefix('?') {
        return (stripped.trim().to_string(), true);
    }
    (trimmed.to_string(), false)
}

fn resolve_expression(
    expression: &str,
    context: &Value,
    missing: &str,
) -> Result<Value, ToolError> {
    let (path, optional) = normalize_expression(expression);
    if path.is_empty() {
        return Ok(Value::String(String::new()));
    }
    match get_path_value(context, &path, true, None) {
        Ok(value) => Ok(value),
        Err(err) => {
            if optional || missing == "empty" {
                Ok(Value::String(String::new()))
            } else if missing == "null" || missing == "undefined" {
                Ok(Value::Null)
            } else {
                Err(err)
            }
        }
    }
}

fn stringify_resolved(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Number(num) => num.to_string(),
        Value::Bool(flag) => flag.to_string(),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

pub fn resolve_template_string(
    template: &str,
    context: &Value,
    missing: &str,
) -> Result<Value, ToolError> {
    let raw = template.to_string();
    if let Some(inner) = raw.strip_prefix("{{").and_then(|s| s.strip_suffix("}}")) {
        let value = resolve_expression(inner, context, missing)?;
        return Ok(value);
    }

    let mut out = String::new();
    let mut rest = raw.as_str();
    while let Some(start) = rest.find("{{") {
        let (prefix, tail) = rest.split_at(start);
        out.push_str(prefix);
        if let Some(end) = tail.find("}}") {
            let expr = &tail[2..end];
            let value = resolve_expression(expr, context, missing)?;
            out.push_str(&stringify_resolved(&value));
            rest = &tail[end + 2..];
        } else {
            out.push_str(tail);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    Ok(Value::String(out))
}

pub fn resolve_templates(
    value: &Value,
    context: &Value,
    missing: &str,
) -> Result<Value, ToolError> {
    match value {
        Value::Array(items) => {
            let mut out = Vec::new();
            for item in items {
                out.push(resolve_templates(item, context, missing)?);
            }
            Ok(Value::Array(out))
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, entry) in map.iter() {
                out.insert(key.clone(), resolve_templates(entry, context, missing)?);
            }
            Ok(Value::Object(out))
        }
        Value::String(text) => resolve_template_string(text, context, missing),
        _ => Ok(value.clone()),
    }
}
