use crate::errors::ToolError;
use crate::utils::merge::merge_deep;
use serde_json::Value;

fn dsl_invalid(message: &str) -> ToolError {
    ToolError::invalid_params(message.to_string()).with_hint(
        "DSL directives: runbook, description, step, tool, action, args, arg, when, foreach, continue_on_error.".to_string(),
    )
}

fn parse_value(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Value::String(String::new());
    }
    if trimmed == "true" || trimmed == "false" || trimmed == "null" {
        return serde_json::from_str(trimmed).unwrap_or(Value::String(trimmed.to_string()));
    }
    if trimmed.parse::<f64>().is_ok() {
        return serde_json::from_str(trimmed).unwrap_or(Value::String(trimmed.to_string()));
    }
    if (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        || (trimmed.starts_with('"') && trimmed.ends_with('"'))
    {
        return serde_json::from_str(trimmed).unwrap_or(Value::String(trimmed.to_string()));
    }
    Value::String(trimmed.to_string())
}

fn set_path(target: &mut Value, path: &str, value: Value) {
    let parts: Vec<String> = path
        .split('.')
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect();
    if parts.is_empty() {
        return;
    }
    let mut current = target;
    let (last, parents) = parts.split_last().unwrap();
    for key in parents {
        if !current.get(key).map(|v| v.is_object()).unwrap_or(false) {
            if let Value::Object(map) = current {
                map.insert(key.clone(), Value::Object(Default::default()));
            }
        }
        current = current.get_mut(key).unwrap();
    }
    if let Value::Object(map) = current {
        map.insert(last.clone(), value);
    }
}

fn parse_key_value(raw: &str) -> Result<(String, Value), ToolError> {
    let trimmed = raw.trim();
    let eq_index = trimmed.find('=');
    let Some(eq_index) = eq_index else {
        return Err(dsl_invalid("arg directive requires key=value"));
    };
    let key = trimmed[..eq_index].trim();
    let value_raw = trimmed[eq_index + 1..].trim();
    if key.is_empty() {
        return Err(dsl_invalid("arg directive requires key=value"));
    }
    Ok((key.to_string(), parse_value(value_raw)))
}

pub fn parse_runbook_dsl(dsl: &str) -> Result<Value, ToolError> {
    let mut runbook = serde_json::json!({ "steps": [] });
    let lines: Vec<&str> = dsl.lines().collect();
    let mut current: Option<Value> = None;

    for (line_index, raw_line) in lines.iter().enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        let mut parts = trimmed.splitn(2, ' ');
        let directive = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim();

        match directive {
            "runbook" => {
                if let Value::Object(map) = &mut runbook {
                    map.insert("name".to_string(), Value::String(rest.to_string()));
                }
            }
            "description" => {
                if let Value::Object(map) = &mut runbook {
                    map.insert("description".to_string(), Value::String(rest.to_string()));
                }
            }
            "step" => {
                let tokens: Vec<&str> = rest.split_whitespace().collect();
                let id = tokens.first().copied().unwrap_or("");
                let tool = tokens.get(1).copied().unwrap_or("");
                if id.is_empty() || tool.is_empty() {
                    return Err(dsl_invalid(&format!(
                        "step requires id and tool at line {}",
                        line_index + 1
                    )));
                }
                let action = tokens.get(2).copied();
                let mut step = serde_json::json!({
                    "id": id,
                    "tool": tool,
                    "args": {}
                });
                if let Some(action) = action {
                    if let Value::Object(map) = step.get_mut("args").unwrap() {
                        map.insert("action".to_string(), Value::String(action.to_string()));
                    }
                }
                if tokens.len() > 3 {
                    let parsed = parse_value(&tokens[3..].join(" "));
                    if parsed.is_object() {
                        let merged = merge_deep(step.get("args").unwrap(), &parsed);
                        if let Value::Object(map) = &mut step {
                            map.insert("args".to_string(), merged);
                        }
                    }
                }
                if let Value::Array(steps) = runbook.get_mut("steps").unwrap() {
                    steps.push(step.clone());
                }
                current = Some(step);
            }
            "tool" => {
                let Some(step) = current.as_mut() else {
                    return Err(dsl_invalid(&format!(
                        "tool directive before step at line {}",
                        line_index + 1
                    )));
                };
                if let Value::Object(map) = step {
                    map.insert("tool".to_string(), Value::String(rest.to_string()));
                }
            }
            "action" => {
                let Some(step) = current.as_mut() else {
                    return Err(dsl_invalid(&format!(
                        "action directive before step at line {}",
                        line_index + 1
                    )));
                };
                if let Some(Value::Object(map)) = step.get_mut("args") {
                    map.insert("action".to_string(), Value::String(rest.to_string()));
                }
            }
            "args" => {
                let Some(step) = current.as_mut() else {
                    return Err(dsl_invalid(&format!(
                        "args directive before step at line {}",
                        line_index + 1
                    )));
                };
                let parsed = parse_value(rest);
                if !parsed.is_object() {
                    return Err(dsl_invalid(&format!(
                        "args directive expects JSON object at line {}",
                        line_index + 1
                    )));
                }
                let merged = merge_deep(step.get("args").unwrap(), &parsed);
                if let Value::Object(map) = step {
                    map.insert("args".to_string(), merged);
                }
            }
            "arg" => {
                let Some(step) = current.as_mut() else {
                    return Err(dsl_invalid(&format!(
                        "arg directive before step at line {}",
                        line_index + 1
                    )));
                };
                let (key, value) = parse_key_value(rest)?;
                if let Some(args) = step.get_mut("args") {
                    set_path(args, &key, value);
                }
            }
            "when" => {
                let Some(step) = current.as_mut() else {
                    return Err(dsl_invalid(&format!(
                        "when directive before step at line {}",
                        line_index + 1
                    )));
                };
                let parsed = parse_value(rest);
                if let Value::Object(map) = step {
                    map.insert("when".to_string(), parsed);
                }
            }
            "foreach" => {
                let Some(step) = current.as_mut() else {
                    return Err(dsl_invalid(&format!(
                        "foreach directive before step at line {}",
                        line_index + 1
                    )));
                };
                let parsed = parse_value(rest);
                if let Value::Object(map) = step {
                    map.insert("foreach".to_string(), parsed);
                }
            }
            "continue_on_error" => {
                let Some(step) = current.as_mut() else {
                    return Err(dsl_invalid(&format!(
                        "continue_on_error directive before step at line {}",
                        line_index + 1
                    )));
                };
                let parsed = parse_value(rest);
                if let Value::Object(map) = step {
                    map.insert(
                        "continue_on_error".to_string(),
                        Value::Bool(parsed == Value::Bool(true)),
                    );
                }
            }
            _ => {
                return Err(dsl_invalid(&format!(
                    "Unknown DSL directive '{}' at line {}",
                    directive,
                    line_index + 1
                )));
            }
        }
    }

    if !runbook
        .get("steps")
        .and_then(|v| v.as_array())
        .map(|arr| !arr.is_empty())
        .unwrap_or(false)
    {
        return Err(dsl_invalid("runbook DSL must define at least one step"));
    }

    Ok(runbook)
}
