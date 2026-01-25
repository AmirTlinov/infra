use crate::errors::ToolError;
use crate::utils::data_path::get_path_value;
use serde_json::Value;

fn resolve_empty_default(output: &Value) -> Value {
    if let Some(obj) = output.as_object() {
        if obj.contains_key("map") {
            return Value::Array(vec![]);
        }
        if obj.contains_key("pick") || obj.contains_key("omit") {
            return Value::Object(Default::default());
        }
    }
    Value::Object(Default::default())
}

fn resolve_missing_default(output: &Value) -> Option<Value> {
    if let Some(obj) = output.as_object() {
        if obj.contains_key("default") {
            return obj.get("default").cloned();
        }
        match obj.get("missing").and_then(|v| v.as_str()) {
            Some("null") => return Some(Value::Null),
            Some("undefined") => return Some(Value::Null),
            Some("empty") => return Some(resolve_empty_default(output)),
            _ => {}
        }
    }
    None
}

fn pick_fields(value: &Value, fields: &[String]) -> Value {
    if let Some(map) = value.as_object() {
        let mut out = serde_json::Map::new();
        for field in fields {
            if let Some(entry) = map.get(field) {
                out.insert(field.clone(), entry.clone());
            }
        }
        Value::Object(out)
    } else {
        value.clone()
    }
}

fn omit_fields(value: &Value, fields: &[String]) -> Value {
    if let Some(map) = value.as_object() {
        let mut out = map.clone();
        for field in fields {
            out.remove(field);
        }
        Value::Object(out)
    } else {
        value.clone()
    }
}

pub fn apply_output_transform(value: &Value, output: Option<&Value>) -> Result<Value, ToolError> {
    let Some(output) = output else {
        return Ok(value.clone());
    };
    let Some(obj) = output.as_object() else {
        return Ok(value.clone());
    };

    let missing_mode = obj
        .get("missing")
        .and_then(|v| v.as_str())
        .unwrap_or("error");
    let required = missing_mode == "error";
    let default_value = resolve_missing_default(output);

    let mut current = value.clone();
    if let Some(path) = obj.get("path").and_then(|v| v.as_str()) {
        current = get_path_value(&current, path, required, default_value.clone())?;
    }

    if let Some(pick) = obj.get("pick").and_then(|v| v.as_array()) {
        let fields: Vec<String> = pick
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        current = pick_fields(&current, &fields);
    }

    if let Some(omit) = obj.get("omit").and_then(|v| v.as_array()) {
        let fields: Vec<String> = omit
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        current = omit_fields(&current, &fields);
    }

    if let Some(map) = obj.get("map") {
        if let Some(items) = current.as_array() {
            let mut out = Vec::new();
            for item in items {
                out.push(apply_output_transform(item, Some(map))?);
            }
            current = Value::Array(out);
        } else if required {
            return Err(ToolError::invalid_params(
                "Output map expects an array result",
            ));
        } else {
            current = default_value.unwrap_or(Value::Null);
        }
    }

    Ok(current)
}
