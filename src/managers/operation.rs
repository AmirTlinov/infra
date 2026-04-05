use crate::errors::ToolError;
use crate::managers::intent::IntentManager;
use crate::services::capability::CapabilityService;
use crate::services::context::ContextService;
use crate::services::description::DescriptionService;
use crate::services::job::JobService;
use crate::services::logger::Logger;
use crate::services::operation::OperationService;
use crate::services::runbook::RunbookService;
use crate::services::validation::Validation;
use crate::utils::checks::{checks_from_args, evaluate_checks};
use crate::utils::manifests::manifest_ref;
use crate::utils::operation_view::derive_live_operation;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

pub(crate) const OPERATION_ACTIONS: &[&str] = &[
    "observe", "plan", "apply", "verify", "rollback", "status", "cancel", "list",
];

#[derive(Clone)]
pub struct OperationManager {
    logger: Logger,
    validation: Validation,
    capability_service: Arc<CapabilityService>,
    runbook_service: Arc<RunbookService>,
    context_service: Option<Arc<ContextService>>,
    intent_manager: Arc<IntentManager>,
    operation_service: Arc<OperationService>,
    job_service: Arc<JobService>,
}

impl OperationManager {
    pub fn new(
        logger: Logger,
        validation: Validation,
        capability_service: Arc<CapabilityService>,
        runbook_service: Arc<RunbookService>,
        context_service: Option<Arc<ContextService>>,
        intent_manager: Arc<IntentManager>,
        operation_service: Arc<OperationService>,
        job_service: Arc<JobService>,
    ) -> Self {
        Self {
            logger: logger.child("operation"),
            validation,
            capability_service,
            runbook_service,
            context_service,
            intent_manager,
            operation_service,
            job_service,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "observe" | "plan" | "apply" | "verify" | "rollback" => {
                self.execute_operation(args).await
            }
            "status" => self.operation_status(args).await,
            "cancel" => self.operation_cancel(args).await,
            "list" => self.operation_list(args).await,
            _ => Err(unknown_action_error("operation", action, OPERATION_ACTIONS)),
        }
    }

    async fn execute_operation(&self, args: Value) -> Result<Value, ToolError> {
        let action = self.validation.ensure_string(
            args.get("action").unwrap_or(&Value::Null),
            "Operation action",
            true,
        )?;
        let trace_id = args
            .get("trace_id")
            .cloned()
            .unwrap_or_else(|| Value::String(uuid::Uuid::new_v4().to_string()));
        let span_id = args
            .get("span_id")
            .cloned()
            .unwrap_or_else(|| Value::String(uuid::Uuid::new_v4().to_string()));
        let parent_span_id = args.get("parent_span_id").cloned().unwrap_or(Value::Null);
        let mut execution_args = args.clone();
        if let Value::Object(map) = &mut execution_args {
            map.insert("trace_id".to_string(), trace_id.clone());
            map.insert("span_id".to_string(), span_id.clone());
            map.insert("parent_span_id".to_string(), parent_span_id.clone());
        }
        let started_at = chrono::Utc::now().to_rfc3339();
        let operation_id = args
            .get("operation_id")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let context = self.resolve_operation_context(&args).await;
        let rollback_source = if action == "rollback" {
            Some(self.rollback_source(&args)?)
        } else {
            None
        };
        let capability = self.resolve_operation_capability(
            &action,
            &args,
            context.as_ref(),
            rollback_source.as_ref(),
        )?;
        let capability_name = capability
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let intent_type = capability
            .get("intent")
            .and_then(|v| v.as_str())
            .unwrap_or(capability_name.as_str())
            .to_string();

        let details = if action == "plan" {
            self.intent_manager
                .handle_action(self.build_intent_args(
                    &action,
                    &execution_args,
                    &intent_type,
                    false,
                ))
                .await?
        } else {
            self.guard_execution_action(&action, &capability)?;
            self.intent_manager
                .handle_action(self.build_intent_args(
                    &action,
                    &execution_args,
                    &intent_type,
                    action == "apply" || action == "rollback",
                ))
                .await?
        };
        let verification = if action == "verify" {
            Some(evaluate_checks(&details, &checks_from_args(&args)?)?)
        } else {
            None
        };

        let finished_at = chrono::Utc::now().to_rfc3339();
        let description_snapshot = DescriptionService::snapshot(
            self.capability_service.as_ref(),
            self.runbook_service.as_ref(),
        )?;
        let missing = details
            .get("missing")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let raw_success = if action == "plan" {
            missing.is_empty()
                && details
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        } else if action == "verify" {
            verification
                .as_ref()
                .and_then(|value| value.get("passed"))
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        } else {
            details
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        };
        let status = if action == "plan" {
            if missing.is_empty() {
                "planned"
            } else {
                "blocked"
            }
        } else if action == "verify" && !raw_success {
            "verify_failed"
        } else if raw_success {
            "completed"
        } else {
            "failed"
        };
        let runbook_name = capability
            .get("runbook")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let runbook_manifest = extract_named_manifest(
            &details,
            "runbook_manifest",
            &capability_name,
            &runbook_name,
        );
        let runbook_manifests = collect_step_manifests(&details, "runbook_manifest");

        let receipt = serde_json::json!({
            "operation_id": operation_id,
            "status": status,
            "action": action,
            "family": args.get("family").cloned().unwrap_or(Value::Null),
            "capability": capability_name,
            "capability_manifest": manifest_ref(&capability),
            "intent": intent_type,
            "runbook": capability.get("runbook").cloned().unwrap_or(Value::Null),
            "runbook_manifest": runbook_manifest,
            "runbook_manifests": runbook_manifests,
            "effects": details.get("plan")
                .and_then(|v| v.get("effects"))
                .cloned()
                .or_else(|| details.get("effects").cloned())
                .or_else(|| capability.get("effects").cloned())
                .unwrap_or(Value::Null),
            "result_success": raw_success,
            "success": raw_success,
            "created_at": started_at,
            "updated_at": finished_at,
            "finished_at": finished_at,
            "trace_id": trace_id,
            "span_id": span_id,
            "parent_span_id": parent_span_id,
            "job_ids": Value::Array(collect_job_ids(&details).into_iter().map(Value::String).collect()),
            "missing": Value::Array(missing),
            "summary": summarize_operation_details(&details),
            "verification": verification.clone().unwrap_or(Value::Null),
            "evidence_summary": evidence_summary(&details),
            "evidence_path": details.get("evidence_path").cloned().unwrap_or(Value::Null),
            "step_trace": collect_step_trace(&details),
            "description_snapshot": description_snapshot,
            "rollback_of": rollback_source
                .as_ref()
                .and_then(|value| value.get("operation_id"))
                .cloned()
                .unwrap_or(Value::Null),
            "rollback_source_summary": rollback_source
                .as_ref()
                .and_then(|value| value.get("summary"))
                .cloned()
                .unwrap_or(Value::Null),
        });
        let live_receipt = derive_live_operation(&receipt, self.job_service.as_ref());
        self.operation_service
            .upsert(&operation_id, &live_receipt)?;

        Ok(serde_json::json!({
            "success": live_receipt.get("success").and_then(|v| v.as_bool()).unwrap_or(false),
            "operation": live_receipt,
            "details": details,
            "effects": capability.get("effects").cloned().unwrap_or(Value::Null),
            "resolved_capability": capability,
            "resolved_runbook_manifest": extract_named_manifest(&details, "runbook_manifest", &capability_name, &runbook_name),
            "manifest": self.capability_service.manifest_metadata(),
        }))
    }

    async fn operation_status(&self, args: Value) -> Result<Value, ToolError> {
        let operation_id = self.ensure_operation_id(&args)?;
        let operation = self.operation_service.get(&operation_id)?;
        let Some(operation) = operation else {
            return Ok(serde_json::json!({
                "success": false,
                "code": "NOT_FOUND",
                "operation_id": operation_id,
            }));
        };
        let live = derive_live_operation(&operation, self.job_service.as_ref());
        self.operation_service.upsert(&operation_id, &live)?;
        Ok(serde_json::json!({"success": true, "operation": live}))
    }

    async fn operation_cancel(&self, args: Value) -> Result<Value, ToolError> {
        let operation_id = self.ensure_operation_id(&args)?;
        let operation = self.operation_service.get(&operation_id)?;
        let Some(mut operation) = operation else {
            return Ok(serde_json::json!({
                "success": false,
                "code": "NOT_FOUND",
                "operation_id": operation_id,
            }));
        };

        let job_ids = operation
            .get("job_ids")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect::<Vec<_>>();
        if job_ids.is_empty() {
            return Ok(serde_json::json!({
                "success": false,
                "code": "NOT_SUPPORTED",
                "operation_id": operation_id,
                "message": "Operation does not reference cancelable jobs",
            }));
        }

        let reason = args.get("reason").and_then(|v| v.as_str());
        let mut canceled_jobs = Vec::new();
        for job_id in job_ids {
            if let Some(canceled) = self.job_service.cancel(&job_id, reason) {
                canceled_jobs.push(canceled);
            }
        }

        if let Value::Object(map) = &mut operation {
            map.insert("status".to_string(), Value::String("canceled".to_string()));
            map.insert("success".to_string(), Value::Bool(false));
            map.insert(
                "updated_at".to_string(),
                Value::String(chrono::Utc::now().to_rfc3339()),
            );
            map.insert(
                "finished_at".to_string(),
                Value::String(chrono::Utc::now().to_rfc3339()),
            );
        }
        self.operation_service.upsert(&operation_id, &operation)?;

        Ok(serde_json::json!({
            "success": true,
            "operation": operation,
            "jobs": canceled_jobs,
        }))
    }

    async fn operation_list(&self, args: Value) -> Result<Value, ToolError> {
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .clamp(1, 100) as usize;
        let status = args.get("status").and_then(|v| v.as_str());
        let operations = self
            .operation_service
            .list(usize::MAX, None)?
            .into_iter()
            .map(|operation| derive_live_operation(&operation, self.job_service.as_ref()))
            .filter(|operation| {
                status
                    .map(|status_filter| {
                        operation.get("status").and_then(|value| value.as_str())
                            == Some(status_filter)
                    })
                    .unwrap_or(true)
            })
            .take(limit)
            .collect::<Vec<_>>();
        Ok(serde_json::json!({
            "success": true,
            "operations": operations,
        }))
    }

    async fn resolve_operation_context(&self, args: &Value) -> Option<Value> {
        let service = self.context_service.as_ref()?;
        service
            .get_context(args)
            .await
            .ok()
            .and_then(|value| value.get("context").cloned())
    }

    fn resolve_operation_capability(
        &self,
        action: &str,
        args: &Value,
        context: Option<&Value>,
        rollback_source: Option<&Value>,
    ) -> Result<Value, ToolError> {
        if let Some(capability_name) = args
            .get("capability")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return self.capability_service.get_capability(capability_name);
        }
        if let Some(intent_type) = args
            .get("intent")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return self
                .capability_service
                .resolve_by_intent(intent_type, context);
        }
        if action == "rollback" {
            let family = args
                .get("family")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string())
                .or_else(|| {
                    rollback_source
                        .and_then(|value| value.get("family"))
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string())
                })
                .ok_or_else(|| {
                    ToolError::new(
                        crate::errors::ToolErrorKind::Conflict,
                        "NOT_ROLLBACKABLE",
                        "rollback requires family or rollback source family",
                    )
                })?;
            return self
                .capability_service
                .resolve_for_operation(&family, action, context);
        }
        let family = self.validation.ensure_string(
            args.get("family").unwrap_or(&Value::Null),
            "Operation family",
            true,
        )?;
        self.capability_service
            .resolve_for_operation(&family, action, context)
    }

    fn build_intent_args(
        &self,
        action: &str,
        args: &Value,
        intent_type: &str,
        apply: bool,
    ) -> Value {
        let intent_action = if action == "plan" {
            "compile"
        } else {
            "execute"
        };
        let mut inputs = args
            .get("input")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        for (key, value) in args.as_object().cloned().unwrap_or_default() {
            if is_operation_control_key(&key) || value.is_null() {
                continue;
            }
            inputs.entry(key).or_insert(value);
        }

        serde_json::json!({
            "action": intent_action,
            "intent": {
                "type": intent_type,
                "inputs": Value::Object(inputs),
            },
            "apply": apply || args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false),
            "confirm": args.get("confirm").and_then(|v| v.as_bool()).unwrap_or(false),
            "save_evidence": args.get("save_evidence").and_then(|v| v.as_bool()).unwrap_or(false),
            "stop_on_error": args.get("stop_on_error").and_then(|v| v.as_bool()).unwrap_or(true),
            "template_missing": args.get("template_missing").cloned().unwrap_or(Value::String("error".to_string())),
            "trace_id": args.get("trace_id").cloned().unwrap_or(Value::String(uuid::Uuid::new_v4().to_string())),
            "span_id": args.get("span_id").cloned().unwrap_or(Value::String(uuid::Uuid::new_v4().to_string())),
            "parent_span_id": args.get("parent_span_id").cloned().unwrap_or(Value::Null),
            "cwd": args.get("cwd").cloned().unwrap_or(Value::Null),
            "repo_root": args.get("repo_root").cloned().unwrap_or(Value::Null),
            "project": args.get("project").cloned().unwrap_or(args.get("project_name").cloned().unwrap_or(Value::Null)),
            "target": args.get("target").cloned().unwrap_or(args.get("project_target").cloned().unwrap_or(Value::Null)),
            "context_key": args.get("context_key").cloned().unwrap_or(Value::Null),
            "context_refresh": args.get("refresh").cloned().unwrap_or(Value::Bool(false)),
        })
    }

    fn guard_execution_action(&self, action: &str, capability: &Value) -> Result<(), ToolError> {
        let effects = capability.get("effects").cloned().unwrap_or(Value::Null);
        let kind = effects
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("read");
        let requires_apply = effects
            .get("requires_apply")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        match action {
            "observe" | "verify" if requires_apply || kind == "write" || kind == "mixed" => {
                Err(ToolError::invalid_params(format!(
                    "Operation action '{}' requires a read capability, got kind='{}'",
                    action, kind
                )))
            }
            _ => Ok(()),
        }
    }

    fn ensure_operation_id(&self, args: &Value) -> Result<String, ToolError> {
        self.validation.ensure_string(
            args.get("operation_id").unwrap_or(&Value::Null),
            "Operation id",
            true,
        )
    }
}

fn is_operation_control_key(key: &str) -> bool {
    matches!(
        key,
        "action"
            | "operation_id"
            | "family"
            | "capability"
            | "intent"
            | "input"
            | "apply"
            | "confirm"
            | "save_evidence"
            | "stop_on_error"
            | "template_missing"
            | "trace_id"
            | "span_id"
            | "parent_span_id"
            | "refresh"
            | "limit"
            | "status"
            | "reason"
            | "checks"
            | "verify"
            | "from_operation_id"
    )
}

fn collect_job_ids(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_job_ids_inner(value, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_job_ids_inner(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map {
                if key == "job_id" {
                    if let Some(job_id) = entry.as_str() {
                        out.push(job_id.to_string());
                    }
                }
                collect_job_ids_inner(entry, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_job_ids_inner(item, out);
            }
        }
        _ => {}
    }
}

fn extract_first_manifest(details: &Value, key: &str) -> Value {
    for path in ["plan.steps", "results"] {
        let Some(items) = value_at_path(details, path).and_then(|value| value.as_array()) else {
            continue;
        };
        for item in items {
            if let Some(manifest) = item.get(key) {
                if !manifest.is_null() {
                    return manifest.clone();
                }
            }
            if let Some(manifest) = item.get("result").and_then(|value| value.get(key)) {
                if !manifest.is_null() {
                    return manifest.clone();
                }
            }
        }
    }
    Value::Null
}

fn extract_named_manifest(
    details: &Value,
    key: &str,
    capability_name: &str,
    runbook_name: &str,
) -> Value {
    for path in ["plan.steps", "results"] {
        let Some(items) = value_at_path(details, path).and_then(|value| value.as_array()) else {
            continue;
        };
        for item in items {
            let matches_capability =
                item.get("capability").and_then(|value| value.as_str()) == Some(capability_name);
            let matches_runbook =
                item.get("runbook").and_then(|value| value.as_str()) == Some(runbook_name);
            if !(matches_capability || matches_runbook) {
                continue;
            }
            if let Some(manifest) = item.get(key) {
                if !manifest.is_null() {
                    return manifest.clone();
                }
            }
            if let Some(manifest) = item.get("result").and_then(|value| value.get(key)) {
                if !manifest.is_null() {
                    return manifest.clone();
                }
            }
        }
    }
    extract_first_manifest(details, key)
}

fn collect_step_manifests(details: &Value, key: &str) -> Value {
    let mut manifests = Vec::new();
    for path in ["plan.steps", "results"] {
        let Some(items) = value_at_path(details, path).and_then(|value| value.as_array()) else {
            continue;
        };
        for item in items {
            for candidate in [
                item.get(key),
                item.get("result").and_then(|value| value.get(key)),
            ] {
                let Some(manifest) = candidate else {
                    continue;
                };
                if manifest.is_null() || manifests.iter().any(|existing| existing == manifest) {
                    continue;
                }
                manifests.push(manifest.clone());
            }
        }
    }
    Value::Array(manifests)
}

fn value_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn summarize_operation_details(details: &Value) -> Value {
    let plan_steps = details
        .get("plan")
        .and_then(|v| v.get("steps"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    let result_steps = details
        .get("results")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    serde_json::json!({
        "success": details.get("success").cloned().unwrap_or(Value::Bool(false)),
        "dry_run": details.get("dry_run").cloned().unwrap_or(Value::Bool(false)),
        "plan_steps": plan_steps,
        "result_steps": result_steps,
        "missing": details.get("missing").cloned().unwrap_or(Value::Array(Vec::new())),
        "evidence_path": details.get("evidence_path").cloned().unwrap_or(Value::Null),
    })
}

fn evidence_summary(details: &Value) -> Value {
    let evidence = details.get("evidence").cloned().unwrap_or(Value::Null);
    if evidence.is_null() {
        return Value::Null;
    }
    serde_json::json!({
        "success": evidence.get("success").cloned().unwrap_or(Value::Null),
        "executed_at": evidence.get("executed_at").cloned().unwrap_or(Value::Null),
        "step_count": evidence
            .get("steps")
            .and_then(|value| value.as_array())
            .map(|items| items.len())
            .unwrap_or(0),
    })
}

fn collect_step_trace(details: &Value) -> Value {
    let steps = details
        .get("results")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|step| {
            serde_json::json!({
                "capability": step.get("capability").cloned().unwrap_or(Value::Null),
                "runbook": step.get("runbook").cloned().unwrap_or(Value::Null),
                "success": step
                    .get("result")
                    .and_then(|value| value.get("success"))
                    .cloned()
                    .unwrap_or(Value::Null),
                "job_ids": Value::Array(collect_job_ids(&step).into_iter().map(Value::String).collect()),
            })
        })
        .collect::<Vec<_>>();
    Value::Array(steps)
}

impl OperationManager {
    fn rollback_source(&self, args: &Value) -> Result<Value, ToolError> {
        let source_id = self.validation.ensure_string(
            args.get("from_operation_id").unwrap_or(&Value::Null),
            "from_operation_id",
            true,
        )?;
        let source = self.operation_service.get(&source_id)?.ok_or_else(|| {
            ToolError::not_found(format!("Source operation not found: {}", source_id))
        })?;
        let source_status = source
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let effect_kind = source
            .get("effects")
            .and_then(|value| value.get("kind"))
            .and_then(|value| value.as_str())
            .unwrap_or("read");
        let requires_apply = source
            .get("effects")
            .and_then(|value| value.get("requires_apply"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if !matches!(source_status, "completed" | "succeeded")
            || (!requires_apply && effect_kind == "read")
        {
            return Err(ToolError::new(
                crate::errors::ToolErrorKind::Conflict,
                "NOT_ROLLBACKABLE",
                "rollback source is not a completed write operation",
            ));
        }
        Ok(source)
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for OperationManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
