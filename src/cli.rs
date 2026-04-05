use crate::app::App;
use crate::errors::{ToolError, ToolErrorKind};
use clap::{Args, Parser, Subcommand};
use serde_json::{Map, Value};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "infra", version, about = "CLI-first canonical infra operator")]
struct Cli {
    #[command(subcommand)]
    command: SurfaceCommand,
}

#[derive(Debug, Subcommand)]
enum SurfaceCommand {
    Describe(RoutedArgs),
    Project(RoutedArgs),
    Target(RoutedArgs),
    Profile(RoutedArgs),
    Capability(RoutedArgs),
    Policy(RoutedArgs),
    Operation(RoutedArgs),
    Receipt(RoutedArgs),
    Job(RoutedArgs),
    Runbook(RoutedArgs),
}

#[derive(Debug, Clone, Args)]
struct RoutedArgs {
    action: String,
    #[arg(long)]
    json: Option<String>,
    #[arg(long = "json-file")]
    json_file: Option<PathBuf>,
    #[arg(long = "arg", value_name = "KEY=VALUE")]
    args: Vec<String>,
}

impl RoutedArgs {
    fn build_payload(&self, action: &str) -> Result<Value, ToolError> {
        let mut payload = Value::Object(Map::new());
        if let Some(path) = &self.json_file {
            let raw = std::fs::read_to_string(path).map_err(|err| {
                ToolError::invalid_params(format!(
                    "failed to read json-file '{}': {}",
                    path.display(),
                    err
                ))
            })?;
            payload = merge_deep(payload, parse_object_json(&raw, "--json-file")?)?;
        }
        if let Some(raw) = &self.json {
            payload = merge_deep(payload, parse_object_json(raw, "--json")?)?;
        }
        for raw in &self.args {
            let (key, value) = raw.split_once('=').ok_or_else(|| {
                ToolError::invalid_params(format!("--arg expects KEY=VALUE, got '{}'", raw))
            })?;
            insert_by_path(&mut payload, key.trim(), parse_cli_value(value.trim())?)?;
        }
        if let Value::Object(map) = &mut payload {
            map.insert("action".to_string(), Value::String(action.to_string()));
        }
        Ok(payload)
    }
}

pub async fn run() -> i32 {
    let cli = Cli::parse();
    let app = match App::initialize() {
        Ok(app) => app,
        Err(err) => return emit_error(Value::Null, None, None, err),
    };
    let snapshot = app.description_snapshot().unwrap_or(Value::Null);

    let (surface, routed) = match cli.command {
        SurfaceCommand::Describe(args) => ("describe", args),
        SurfaceCommand::Project(args) => ("project", args),
        SurfaceCommand::Target(args) => ("target", args),
        SurfaceCommand::Profile(args) => ("profile", args),
        SurfaceCommand::Capability(args) => ("capability", args),
        SurfaceCommand::Policy(args) => ("policy", args),
        SurfaceCommand::Operation(args) => ("operation", args),
        SurfaceCommand::Receipt(args) => ("receipt", args),
        SurfaceCommand::Job(args) => ("job", args),
        SurfaceCommand::Runbook(args) => ("runbook", args),
    };
    let action = normalize_action(surface, routed.action.as_str());
    let payload = match routed.build_payload(&action) {
        Ok(payload) => payload,
        Err(err) => return emit_error(snapshot, Some(surface), Some(&action), err),
    };

    let result = if surface == "describe" {
        handle_describe(&snapshot, &action)
    } else {
        execute_surface(&app, surface, payload).await
    };

    match result {
        Ok(result) => emit_success(snapshot, surface, &action, result),
        Err(err) => emit_error(snapshot, Some(surface), Some(&action), err),
    }
}

fn handle_describe(snapshot: &Value, action: &str) -> Result<Value, ToolError> {
    match action {
        "status" => Ok(serde_json::json!({
            "success": true,
            "active_version": snapshot.get("version").cloned().unwrap_or(Value::Null),
            "active_hash": snapshot.get("hash").cloned().unwrap_or(Value::Null),
            "active_sources": snapshot.get("sources").cloned().unwrap_or(Value::Null),
            "loaded_at": snapshot.get("loaded_at").cloned().unwrap_or(Value::Null),
        })),
        _ => Err(
            ToolError::invalid_params(format!("unknown describe action '{}'", action))
                .with_hint("Use: infra describe status".to_string()),
        ),
    }
}

async fn execute_surface(app: &App, surface: &str, payload: Value) -> Result<Value, ToolError> {
    match surface {
        "project" => app.project_manager.handle_action(payload).await,
        "target" => app.target_manager.handle_action(payload).await,
        "profile" => app.profile_manager.handle_action(payload).await,
        "capability" => app.capability_manager.handle_action(payload).await,
        "policy" => app.policy_manager.handle_action(payload).await,
        "operation" => app.operation_manager.handle_action(payload).await,
        "receipt" => app.receipt_manager.handle_action(payload).await,
        "job" => app.job_manager.handle_action(payload).await,
        "runbook" => app.runbook_manager.handle_action(payload).await,
        _ => Err(ToolError::invalid_params(format!(
            "unknown surface '{}'",
            surface
        ))),
    }
}

fn emit_success(snapshot: Value, surface: &str, action: &str, result: Value) -> i32 {
    let state = derive_state(surface, action, &result, None);
    let ok = exit_code_from_state(&state) == 0;
    let receipt = extract_receipt(&result);
    let summary = build_summary(surface, action, &state, &result);
    print_json(&serde_json::json!({
        "ok": ok,
        "state": state,
        "summary": summary,
        "description_snapshot": snapshot,
        "result": result,
        "receipt": receipt,
    }));
    exit_code_from_state(&state)
}

fn emit_error(snapshot: Value, surface: Option<&str>, action: Option<&str>, err: ToolError) -> i32 {
    let state = error_state(&err);
    print_json(&serde_json::json!({
        "ok": false,
        "state": state,
        "summary": err.message,
        "description_snapshot": snapshot,
        "result": Value::Null,
        "receipt": Value::Null,
        "surface": surface,
        "action": action,
        "error": err,
    }));
    exit_code_from_error(&err)
}

fn build_summary(surface: &str, action: &str, state: &str, result: &Value) -> String {
    if let Some(summary) = result
        .get("operation")
        .and_then(|value| value.get("summary"))
        .and_then(|value| value.as_str())
    {
        return summary.to_string();
    }
    if let Some(summary) = result
        .get("receipt")
        .and_then(|value| value.get("summary"))
        .and_then(|value| value.as_str())
    {
        return summary.to_string();
    }
    if let Some(project) = result.get("project").and_then(|value| value.as_str()) {
        if let Some(target) = result.get("target_name").and_then(|value| value.as_str()) {
            return format!("{} {} {}::{}", surface, action, project, target);
        }
        return format!("{} {} {}", surface, action, project);
    }
    if let Some(name) = result
        .get("profile")
        .and_then(|value| value.get("name"))
        .and_then(|value| value.as_str())
    {
        return format!("profile {} {}", action, name);
    }
    if let Some(name) = result
        .get("capability")
        .and_then(|value| value.get("name"))
        .and_then(|value| value.as_str())
    {
        return format!("capability {} {}", action, name);
    }
    if let Some(operation_id) = result
        .get("operation")
        .and_then(|value| value.get("operation_id"))
        .and_then(|value| value.as_str())
    {
        return format!("operation {} {} {}", action, operation_id, state);
    }
    if let Some(operation_id) = result
        .get("receipt")
        .and_then(|value| value.get("operation_id"))
        .and_then(|value| value.as_str())
    {
        return format!("receipt {} {} {}", action, operation_id, state);
    }
    if let Some(job_id) = result
        .get("job")
        .and_then(|value| value.get("job_id"))
        .and_then(|value| value.as_str())
    {
        return format!("job {} {} {}", action, job_id, state);
    }
    format!("{} {} {}", surface, action, state)
}

fn extract_receipt(result: &Value) -> Value {
    if let Some(receipt) = result.get("receipt") {
        return receipt.clone();
    }
    if let Some(operation) = result.get("operation") {
        return operation.clone();
    }
    Value::Null
}

fn derive_state(surface: &str, action: &str, result: &Value, ok_override: Option<bool>) -> String {
    if surface == "policy" && action == "check" {
        if result.get("allowed").and_then(|value| value.as_bool()) == Some(false) {
            return "denied".to_string();
        }
        if result
            .get("evaluation")
            .and_then(|value| value.get("allowed"))
            .and_then(|value| value.as_bool())
            == Some(false)
        {
            return "denied".to_string();
        }
    }
    for path in [
        "/operation/status",
        "/receipt/status",
        "/job/status",
        "/wait/status",
        "/status/status",
    ] {
        if let Some(state) = result.pointer(path).and_then(|value| value.as_str()) {
            return state.to_string();
        }
    }
    if action == "status" && surface == "describe" {
        return "ready".to_string();
    }
    let ok = ok_override.unwrap_or_else(|| {
        result
            .get("success")
            .and_then(|value| value.as_bool())
            .unwrap_or(true)
    });
    if ok {
        "completed".to_string()
    } else {
        "failed".to_string()
    }
}

fn error_state(err: &ToolError) -> &'static str {
    match err.code.as_str() {
        "AMBIGUOUS_CAPABILITY" => "ambiguous",
        "NOT_ROLLBACKABLE" => "blocked",
        _ => match err.kind {
            ToolErrorKind::Denied => "denied",
            ToolErrorKind::Conflict => "blocked",
            ToolErrorKind::InvalidParams => "blocked",
            _ => "failed",
        },
    }
}

fn exit_code_from_state(state: &str) -> i32 {
    match state {
        "waiting_external" => 12,
        "verify_failed" => 20,
        "blocked" | "ambiguous" => 30,
        "denied" => 40,
        "failed" => 50,
        _ => 0,
    }
}

fn exit_code_from_error(err: &ToolError) -> i32 {
    match err.code.as_str() {
        "AMBIGUOUS_CAPABILITY" | "NOT_ROLLBACKABLE" => 30,
        _ => match err.kind {
            ToolErrorKind::Denied => 40,
            ToolErrorKind::Conflict | ToolErrorKind::InvalidParams => 30,
            _ => 50,
        },
    }
}

fn normalize_action(surface: &str, action: &str) -> String {
    match (surface, action) {
        ("policy", "evaluate") => "check".to_string(),
        ("runbook", "get") => "runbook_get".to_string(),
        ("runbook", "list") => "runbook_list".to_string(),
        ("runbook", "run") => "runbook_run".to_string(),
        ("project", action) => action.to_string(),
        _ => action.to_string(),
    }
}

fn parse_object_json(raw: &str, source: &str) -> Result<Value, ToolError> {
    let value: Value = serde_json::from_str(raw).map_err(|err| {
        ToolError::invalid_params(format!("failed to parse {} JSON: {}", source, err))
    })?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(ToolError::invalid_params(format!(
            "{} must be a JSON object",
            source
        )))
    }
}

fn parse_cli_value(raw: &str) -> Result<Value, ToolError> {
    if raw.is_empty() {
        return Ok(Value::String(String::new()));
    }
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return Ok(value);
    }
    Ok(Value::String(raw.to_string()))
}

fn merge_deep(base: Value, patch: Value) -> Result<Value, ToolError> {
    match (base, patch) {
        (Value::Object(mut base_map), Value::Object(patch_map)) => {
            for (key, value) in patch_map {
                let existing = base_map.remove(&key).unwrap_or(Value::Null);
                let merged = if existing.is_object() && value.is_object() {
                    merge_deep(existing, value)?
                } else {
                    value
                };
                base_map.insert(key, merged);
            }
            Ok(Value::Object(base_map))
        }
        (_, value) => Ok(value),
    }
}

fn insert_by_path(target: &mut Value, path: &str, value: Value) -> Result<(), ToolError> {
    let segments = path
        .split('.')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        return Err(ToolError::invalid_params("argument key must be non-empty"));
    }
    let mut current = target;
    for (index, segment) in segments.iter().enumerate() {
        let is_last = index + 1 == segments.len();
        if is_last {
            let map = current.as_object_mut().ok_or_else(|| {
                ToolError::invalid_params(format!(
                    "cannot assign '{}' into non-object payload",
                    path
                ))
            })?;
            map.insert((*segment).to_string(), value);
            return Ok(());
        }
        let map = current.as_object_mut().ok_or_else(|| {
            ToolError::invalid_params(format!(
                "cannot assign nested key '{}' into non-object payload",
                path
            ))
        })?;
        current = map
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    Ok(())
}

fn print_json(value: &Value) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(_) => println!(
            r#"{{"ok":false,"state":"failed","summary":"failed to serialize CLI output"}}"#
        ),
    }
}
