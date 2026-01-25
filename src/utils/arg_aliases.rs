use serde_json::{Map, Value};
use std::collections::HashSet;

#[derive(Default)]
struct NormalizationState {
    renamed: Vec<Value>,
    converted: Vec<Value>,
    ignored: Vec<Value>,
}

const RUNBOOK_ACTION_ALIASES: &[(&str, &str)] = &[
    ("list", "runbook_list"),
    ("get", "runbook_get"),
    ("delete", "runbook_delete"),
    ("run", "runbook_run"),
    ("execute", "runbook_run"),
    ("compile", "runbook_compile"),
    ("upsert", "runbook_upsert"),
    ("set", "runbook_upsert"),
    ("upsert_dsl", "runbook_upsert_dsl"),
    ("run_dsl", "runbook_run_dsl"),
];

const ALIAS_ACTION_ALIASES: &[(&str, &str)] = &[
    ("list", "alias_list"),
    ("get", "alias_get"),
    ("delete", "alias_delete"),
    ("upsert", "alias_upsert"),
    ("set", "alias_upsert"),
    ("resolve", "alias_resolve"),
];

const PRESET_ACTION_ALIASES: &[(&str, &str)] = &[
    ("list", "preset_list"),
    ("get", "preset_get"),
    ("delete", "preset_delete"),
    ("upsert", "preset_upsert"),
    ("set", "preset_upsert"),
];

const PROJECT_ACTION_ALIASES: &[(&str, &str)] = &[
    ("list", "project_list"),
    ("get", "project_get"),
    ("delete", "project_delete"),
    ("upsert", "project_upsert"),
    ("set", "project_upsert"),
    ("use", "project_use"),
    ("active", "project_active"),
    ("unuse", "project_unuse"),
];

const AUDIT_ACTION_ALIASES: &[(&str, &str)] = &[
    ("list", "audit_list"),
    ("tail", "audit_tail"),
    ("clear", "audit_clear"),
    ("stats", "audit_stats"),
];

const JOB_ACTION_ALIASES: &[(&str, &str)] = &[
    ("list", "job_list"),
    ("status", "job_status"),
    ("get", "job_status"),
    ("wait", "job_wait"),
    ("tail", "job_logs_tail"),
    ("logs", "job_logs_tail"),
    ("follow", "follow_job"),
    ("cancel", "job_cancel"),
    ("forget", "job_forget"),
];

fn action_aliases(tool: &str) -> &'static [(&'static str, &'static str)] {
    match tool {
        "mcp_runbook" => RUNBOOK_ACTION_ALIASES,
        "mcp_alias" => ALIAS_ACTION_ALIASES,
        "mcp_preset" => PRESET_ACTION_ALIASES,
        "mcp_project" => PROJECT_ACTION_ALIASES,
        "mcp_audit" => AUDIT_ACTION_ALIASES,
        "mcp_jobs" => JOB_ACTION_ALIASES,
        _ => &[],
    }
}

fn resolve_action_alias(tool: &str, action: &str) -> Option<&'static str> {
    for (alias, canonical) in action_aliases(tool) {
        if alias == &action {
            return Some(*canonical);
        }
    }
    None
}

pub fn action_aliases_for_tool(tool: &str) -> Vec<(String, String)> {
    action_aliases(tool)
        .iter()
        .map(|(alias, canonical)| (alias.to_string(), canonical.to_string()))
        .collect()
}

fn is_plain_object(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

fn has_key(map: &Map<String, Value>, key: &str) -> bool {
    map.contains_key(key)
}

fn is_finite_number(value: &Value) -> bool {
    match value {
        Value::Number(num) => num.as_f64().map(|f| f.is_finite()).unwrap_or(false),
        Value::String(text) => text.parse::<f64>().map(|f| f.is_finite()).unwrap_or(false),
        _ => false,
    }
}

fn to_number(value: &Value) -> f64 {
    match value {
        Value::Number(num) => num.as_f64().unwrap_or(0.0),
        Value::String(text) => text.parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn rename_key(
    map: &mut Map<String, Value>,
    from_key: &str,
    to_key: &str,
    state: &mut NormalizationState,
    allowed_keys: Option<&HashSet<String>>,
    note: Option<&str>,
) {
    if !has_key(map, from_key) {
        return;
    }
    if let Some(allowed) = allowed_keys {
        if !allowed.contains(to_key) {
            return;
        }
    }
    if has_key(map, to_key) {
        map.remove(from_key);
        state.ignored.push(serde_json::json!({
            "from": from_key,
            "to": to_key,
            "reason": "canonical_already_set",
            "note": note,
        }));
        return;
    }
    if let Some(value) = map.remove(from_key) {
        map.insert(to_key.to_string(), value);
        state.renamed.push(serde_json::json!({
            "from": from_key,
            "to": to_key,
            "note": note,
        }));
    }
}

fn convert_seconds_to_ms(
    map: &mut Map<String, Value>,
    from_key: &str,
    to_key: &str,
    state: &mut NormalizationState,
    allowed_keys: Option<&HashSet<String>>,
    note: Option<&str>,
) {
    if !has_key(map, from_key) {
        return;
    }
    if let Some(allowed) = allowed_keys {
        if !allowed.contains(to_key) {
            return;
        }
    }
    if has_key(map, to_key) {
        map.remove(from_key);
        state.ignored.push(serde_json::json!({
            "from": from_key,
            "to": to_key,
            "reason": "canonical_already_set",
            "note": note,
        }));
        return;
    }
    let raw = map.remove(from_key).unwrap_or(Value::Null);
    let converted = if is_finite_number(&raw) {
        Value::Number(serde_json::Number::from(
            (to_number(&raw) * 1000.0).floor() as i64
        ))
    } else {
        raw.clone()
    };
    map.insert(to_key.to_string(), converted);
    state.converted.push(serde_json::json!({
        "from": from_key,
        "to": to_key,
        "op": "seconds_to_ms",
        "factor": 1000,
        "note": note,
    }));
}

fn compact_state(state: NormalizationState) -> Option<Value> {
    let mut out = serde_json::Map::new();
    if !state.renamed.is_empty() {
        out.insert("renamed".to_string(), Value::Array(state.renamed));
    }
    if !state.converted.is_empty() {
        out.insert("converted".to_string(), Value::Array(state.converted));
    }
    if !state.ignored.is_empty() {
        out.insert("ignored".to_string(), Value::Array(state.ignored));
    }
    if out.is_empty() {
        None
    } else {
        Some(Value::Object(out))
    }
}

pub fn normalize_args_aliases(
    args: &Value,
    tool: &str,
    _action: Option<&str>,
    allowed_keys: Option<&HashSet<String>>,
) -> (Value, Option<Value>) {
    if !is_plain_object(args) {
        return (args.clone(), None);
    }
    let mut out = args.as_object().cloned().unwrap_or_default();
    let mut state = NormalizationState::default();

    if let Some(action_raw) = out.get("action").and_then(|v| v.as_str()) {
        let action_raw = action_raw.to_string();
        let normalized = action_raw.trim().to_lowercase();
        if let Some(mapped) = resolve_action_alias(tool, normalized.as_str()) {
            if mapped != action_raw {
                out.insert("action".to_string(), Value::String(mapped.to_string()));
                state.renamed.push(serde_json::json!({
                    "from": action_raw,
                    "to": mapped,
                    "note": "action_alias",
                }));
            }
        }
    }
    let action_value = out
        .get("action")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    rename_key(&mut out, "cmd", "command", &mut state, allowed_keys, None);
    rename_key(&mut out, "argv", "args", &mut state, allowed_keys, None);
    rename_key(&mut out, "workdir", "cwd", &mut state, allowed_keys, None);
    rename_key(&mut out, "work_dir", "cwd", &mut state, allowed_keys, None);

    rename_key(
        &mut out,
        "timeout",
        "timeout_ms",
        &mut state,
        allowed_keys,
        None,
    );
    rename_key(
        &mut out,
        "timeoutMs",
        "timeout_ms",
        &mut state,
        allowed_keys,
        None,
    );
    convert_seconds_to_ms(
        &mut out,
        "timeout_s",
        "timeout_ms",
        &mut state,
        allowed_keys,
        None,
    );

    rename_key(
        &mut out,
        "profile",
        "profile_name",
        &mut state,
        allowed_keys,
        None,
    );
    rename_key(
        &mut out,
        "profileName",
        "profile_name",
        &mut state,
        allowed_keys,
        None,
    );

    if matches!(
        tool,
        "mcp_runbook" | "mcp_alias" | "mcp_preset" | "mcp_project" | "mcp_capability"
    ) {
        rename_key(
            &mut out,
            "id",
            "name",
            &mut state,
            allowed_keys,
            Some("id alias"),
        );
    }
    if tool == "mcp_jobs" {
        rename_key(
            &mut out,
            "id",
            "job_id",
            &mut state,
            allowed_keys,
            Some("job id alias"),
        );
    }

    if let Some(action) = action_value.as_deref() {
        if action.starts_with("profile_") {
            rename_key(
                &mut out,
                "name",
                "profile_name",
                &mut state,
                allowed_keys,
                Some("profile_* sugar"),
            );
        }
    }

    if tool == "help" {
        rename_key(&mut out, "q", "query", &mut state, allowed_keys, None);
    }

    if matches!(
        tool,
        "mcp_runbook"
            | "mcp_alias"
            | "mcp_preset"
            | "mcp_project"
            | "mcp_capability"
            | "mcp_evidence"
    ) {
        rename_key(
            &mut out,
            "q",
            "query",
            &mut state,
            allowed_keys,
            Some("query alias"),
        );
    }
    if tool == "mcp_runbook" || tool == "mcp_capability" {
        rename_key(
            &mut out,
            "tag",
            "tags",
            &mut state,
            allowed_keys,
            Some("tags alias"),
        );
    }

    if tool == "mcp_api_client" {
        rename_key(&mut out, "params", "query", &mut state, allowed_keys, None);
    }

    if tool == "mcp_psql_manager" {
        if let Some(action) = action_value.as_deref() {
            if action == "query" {
                rename_key(
                    &mut out,
                    "query",
                    "sql",
                    &mut state,
                    allowed_keys,
                    Some("query action expects sql"),
                );
            }
        }
    }

    if tool == "mcp_jobs" {
        rename_key(
            &mut out,
            "poll_interval",
            "poll_interval_ms",
            &mut state,
            allowed_keys,
            None,
        );
        convert_seconds_to_ms(
            &mut out,
            "poll_interval_s",
            "poll_interval_ms",
            &mut state,
            allowed_keys,
            None,
        );
    }

    if tool == "mcp_pipeline" {
        convert_seconds_to_ms(
            &mut out,
            "settle_s",
            "settle_ms",
            &mut state,
            allowed_keys,
            None,
        );
        convert_seconds_to_ms(
            &mut out,
            "smoke_delay_s",
            "smoke_delay_ms",
            &mut state,
            allowed_keys,
            None,
        );
        convert_seconds_to_ms(
            &mut out,
            "smoke_timeout_s",
            "smoke_timeout_ms",
            &mut state,
            allowed_keys,
            None,
        );
    }

    if tool == "mcp_ssh_manager" {
        rename_key(
            &mut out,
            "start_timeout",
            "start_timeout_ms",
            &mut state,
            allowed_keys,
            None,
        );
        convert_seconds_to_ms(
            &mut out,
            "start_timeout_s",
            "start_timeout_ms",
            &mut state,
            allowed_keys,
            None,
        );
    }

    (Value::Object(out), compact_state(state))
}

#[cfg(test)]
mod tests {
    use super::normalize_args_aliases;

    #[test]
    fn normalize_action_aliases_for_runbook() {
        let args = serde_json::json!({"action": "list"});
        let (normalized, _) = normalize_args_aliases(&args, "mcp_runbook", None, None);
        assert_eq!(
            normalized.get("action").and_then(|v| v.as_str()),
            Some("runbook_list")
        );
    }

    #[test]
    fn normalize_id_aliases() {
        let args = serde_json::json!({"action": "runbook_get", "id": "rb"});
        let (normalized, _) = normalize_args_aliases(&args, "mcp_runbook", None, None);
        assert!(normalized.get("id").is_none());
        assert_eq!(normalized.get("name").and_then(|v| v.as_str()), Some("rb"));
    }

    #[test]
    fn normalize_query_alias_for_capability() {
        let args = serde_json::json!({"action": "list", "q": "k8s"});
        let (normalized, _) = normalize_args_aliases(&args, "mcp_capability", None, None);
        assert!(normalized.get("q").is_none());
        assert_eq!(
            normalized.get("query").and_then(|v| v.as_str()),
            Some("k8s")
        );
    }
}
