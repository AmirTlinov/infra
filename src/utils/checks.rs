use crate::errors::ToolError;
use serde_json::Value;

pub fn checks_from_args(args: &Value) -> Result<Vec<Value>, ToolError> {
    let checks = args
        .get("checks")
        .or_else(|| args.get("verify").and_then(|value| value.get("checks")))
        .or_else(|| args.get("input").and_then(|value| value.get("checks")))
        .cloned()
        .unwrap_or(Value::Null);
    if checks.is_null() {
        return Err(ToolError::invalid_params(
            "verify requires explicit checks",
        )
        .with_hint(
            "Pass checks=[{ path: \"results.0.result.success\", equals: true }] to make verify a strict contract."
                .to_string(),
        ));
    }
    let array = checks
        .as_array()
        .cloned()
        .ok_or_else(|| ToolError::invalid_params("checks must be an array of check objects"))?;
    if array.is_empty() {
        return Err(ToolError::invalid_params(
            "verify requires at least one check",
        ));
    }
    Ok(array)
}

pub fn evaluate_checks(subject: &Value, checks: &[Value]) -> Result<Value, ToolError> {
    let mut results = Vec::new();
    let mut passed = true;

    for check in checks {
        let object = check
            .as_object()
            .ok_or_else(|| ToolError::invalid_params("each check must be an object"))?;
        let path = object
            .get("path")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ToolError::invalid_params("check.path must be a non-empty string"))?;
        let actual = value_at_dot_path(subject, path)
            .cloned()
            .unwrap_or(Value::Null);
        let (rule, current_pass, message) = evaluate_single_check(&actual, object)?;
        if !current_pass {
            passed = false;
        }
        results.push(serde_json::json!({
            "path": path,
            "rule": rule,
            "passed": current_pass,
            "actual": actual,
            "message": message,
        }));
    }

    Ok(serde_json::json!({
        "passed": passed,
        "checks": results,
    }))
}

fn evaluate_single_check(
    actual: &Value,
    check: &serde_json::Map<String, Value>,
) -> Result<(Value, bool, String), ToolError> {
    if let Some(expected) = check.get("equals") {
        let ok = actual == expected;
        return Ok((
            serde_json::json!({"equals": expected}),
            ok,
            if ok {
                "value matched equals".to_string()
            } else {
                "value did not match equals".to_string()
            },
        ));
    }
    if let Some(expected) = check.get("not_equals") {
        let ok = actual != expected;
        return Ok((
            serde_json::json!({"not_equals": expected}),
            ok,
            if ok {
                "value matched not_equals".to_string()
            } else {
                "value violated not_equals".to_string()
            },
        ));
    }
    if let Some(exists) = check.get("exists").and_then(|value| value.as_bool()) {
        let actual_exists = !actual.is_null();
        let ok = actual_exists == exists;
        return Ok((
            serde_json::json!({"exists": exists}),
            ok,
            if ok {
                "existence matched".to_string()
            } else {
                "existence mismatched".to_string()
            },
        ));
    }
    if let Some(expected) = check.get("one_of") {
        let array = expected
            .as_array()
            .ok_or_else(|| ToolError::invalid_params("check.one_of must be an array"))?;
        let ok = array.iter().any(|value| value == actual);
        return Ok((
            serde_json::json!({"one_of": expected}),
            ok,
            if ok {
                "value matched one_of".to_string()
            } else {
                "value did not match one_of".to_string()
            },
        ));
    }
    if let Some(expected) = check.get("contains") {
        let ok = match (actual, expected) {
            (Value::String(actual_text), Value::String(expected_text)) => {
                actual_text.contains(expected_text)
            }
            (Value::Array(items), _) => items.iter().any(|item| item == expected),
            _ => false,
        };
        return Ok((
            serde_json::json!({"contains": expected}),
            ok,
            if ok {
                "value matched contains".to_string()
            } else {
                "value did not match contains".to_string()
            },
        ));
    }
    Err(ToolError::invalid_params(
        "each check must specify one of equals, not_equals, exists, one_of, contains",
    ))
}

fn value_at_dot_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let pointer = format!(
        "/{}",
        path.split('.')
            .map(|segment| segment.trim())
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    );
    if pointer == "/" {
        Some(value)
    } else {
        value.pointer(pointer.as_str())
    }
}
