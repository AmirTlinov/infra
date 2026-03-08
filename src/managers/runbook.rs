use crate::errors::ToolError;
use crate::mcp::tool_effects::resolve_tool_call_effects;
use crate::services::logger::Logger;
use crate::services::runbook::RunbookService;
use crate::services::state::StateService;
use crate::services::tool_executor::ToolExecutor;
use crate::utils::data_path::get_path_value;
use crate::utils::effects::{resolve_effects, Effects};
use crate::utils::listing::ListFilters;
use crate::utils::manifests::manifest_ref;
use crate::utils::template::{resolve_template_string, resolve_templates};
use crate::utils::tool_errors::unknown_action_error;
use once_cell::sync::OnceCell;
use serde_json::Value;
use std::sync::{Arc, Weak};

pub(crate) const RUNBOOK_ACTIONS: &[&str] = &[
    "runbook_upsert",
    "runbook_upsert_dsl",
    "runbook_get",
    "runbook_list",
    "runbook_delete",
    "runbook_run",
    "runbook_run_dsl",
    "runbook_compile",
];

fn merge_effects(mut base: Effects, other: Effects) -> Effects {
    let base_kind = base.kind.as_deref().unwrap_or("read");
    let other_kind = other.kind.as_deref().unwrap_or("read");
    let merged_kind = if base_kind == "mixed" || other_kind == "mixed" {
        "mixed"
    } else if base_kind == "write" || other_kind == "write" {
        "write"
    } else {
        "read"
    };
    base.kind = Some(merged_kind.to_string());
    base.requires_apply = base.requires_apply || other.requires_apply;
    base.irreversible = base.irreversible || other.irreversible;
    base
}

fn infer_effects_from_steps(runbook: &Value) -> Effects {
    let steps = runbook
        .get("steps")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Effects {
        kind: Some("read".to_string()),
        requires_apply: false,
        irreversible: false,
    };
    for step in steps.iter() {
        let tool = step.get("tool").and_then(|v| v.as_str()).unwrap_or("");
        if tool.trim().is_empty() {
            continue;
        }
        let args = step
            .get("args")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        let resolved = resolve_tool_call_effects(tool, &args);
        out = merge_effects(out, resolved.effects);
    }
    out
}

#[derive(Clone)]
pub struct RunbookManager {
    logger: Logger,
    runbook_service: Arc<RunbookService>,
    state_service: Arc<StateService>,
    tool_executor: Arc<OnceCell<Weak<ToolExecutor>>>,
}

impl RunbookManager {
    pub fn new(
        logger: Logger,
        runbook_service: Arc<RunbookService>,
        state_service: Arc<StateService>,
    ) -> Self {
        Self {
            logger: logger.child("runbook"),
            runbook_service,
            state_service,
            tool_executor: Arc::new(OnceCell::new()),
        }
    }

    fn manifest_path_display(&self) -> String {
        self.runbook_service.manifest_path().display().to_string()
    }

    fn compatibility_only_error(&self, action: &str, stage: &str) -> ToolError {
        ToolError::invalid_params(format!(
            "{} is compatibility-only and no longer supported in normal mode",
            action
        ))
        .with_hint(format!(
            "Move the runbook definition into {} and execute it via action=runbook_run with name=<runbook-name>.",
            self.manifest_path_display()
        ))
        .with_details(serde_json::json!({
            "stage": stage,
            "action": action,
            "manifest_path": self.manifest_path_display(),
        }))
    }

    fn inline_runbook_error(&self) -> ToolError {
        ToolError::invalid_params(
            "inline runbook payloads are compatibility-only and no longer supported in normal mode",
        )
        .with_hint(format!(
            "Save the runbook in {} and rerun with action=runbook_run plus name=<runbook-name>.",
            self.manifest_path_display()
        ))
        .with_details(serde_json::json!({
            "stage": "compatibility_runbook_inline",
            "manifest_path": self.manifest_path_display(),
        }))
    }

    pub fn set_tool_executor(&self, tool_executor: Arc<ToolExecutor>) {
        let _ = self.tool_executor.set(Arc::downgrade(&tool_executor));
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "runbook_upsert" => {
                Err(self
                    .compatibility_only_error("runbook_upsert", "compatibility_runbook_mutation"))
            }
            "runbook_upsert_dsl" => {
                Err(self
                    .compatibility_only_error("runbook_upsert_dsl", "compatibility_runbook_dsl"))
            }
            "runbook_get" => {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                self.runbook_service.get_runbook(name)
            }
            "runbook_list" => {
                let filters = ListFilters::from_args(&args);
                self.runbook_service.list_runbooks(&filters)
            }
            "runbook_delete" => {
                Err(self
                    .compatibility_only_error("runbook_delete", "compatibility_runbook_mutation"))
            }
            "runbook_compile" => {
                Err(self.compatibility_only_error("runbook_compile", "compatibility_runbook_dsl"))
            }
            "runbook_run" => self.runbook_run(args).await,
            "runbook_run_dsl" => {
                Err(self.compatibility_only_error("runbook_run_dsl", "compatibility_runbook_dsl"))
            }
            _ => Err(unknown_action_error("runbook", action, RUNBOOK_ACTIONS)),
        }
    }

    pub async fn cleanup(&self) -> Result<Value, ToolError> {
        Ok(serde_json::json!({ "success": true }))
    }

    fn resolve_tool_executor(&self) -> Result<Arc<ToolExecutor>, ToolError> {
        self.tool_executor
            .get()
            .and_then(|executor| executor.upgrade())
            .ok_or_else(|| {
                ToolError::internal("Tool executor is not available for runbook execution")
                    .with_hint(
                        "App wiring bug: RunbookManager.set_tool_executor(...) must be called during initialization."
                            .to_string(),
                    )
            })
    }

    async fn runbook_run(&self, args: Value) -> Result<Value, ToolError> {
        let tool_executor = self.resolve_tool_executor()?;
        let input = args
            .get("input")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let stop_on_error = args
            .get("stop_on_error")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let apply = args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false);
        let confirm = args
            .get("confirm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let template_missing = args
            .get("template_missing")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
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

        if let Some(seed) = args.get("seed_state").and_then(|v| v.as_object()) {
            let scope = args
                .get("seed_state_scope")
                .and_then(|v| v.as_str())
                .unwrap_or("session");
            for (key, value) in seed {
                let _ = self.state_service.set(key, value.clone(), Some(scope));
            }
        }

        if args.get("runbook").is_some() {
            return Err(self.inline_runbook_error());
        }

        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ToolError::invalid_params("runbook_run requires a manifest-backed runbook name")
                    .with_hint(
                        "Choose a runbook from action=runbook_list and rerun with that name."
                            .to_string(),
                    )
            })?;
        let runbook = self.runbook_service.resolve_runbook(name)?;

        let steps = runbook
            .get("steps")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if steps.is_empty() {
            return Err(ToolError::invalid_params(
                "runbook.steps must be a non-empty array",
            ));
        }

        let effects = merge_effects(
            resolve_effects(&runbook),
            infer_effects_from_steps(&runbook),
        );
        if effects.requires_apply && !apply {
            return Err(
                ToolError::denied("Runbook requires apply=true for write/mixed effects").with_hint(
                    "Rerun with apply=true if you intend to perform write operations.".to_string(),
                ),
            );
        }
        if effects.irreversible && !confirm {
            return Err(ToolError::denied(
                "Runbook requires confirm=true for irreversible effects",
            )
            .with_hint(
                "Rerun with confirm=true if you understand this cannot be safely auto-rolled-back."
                    .to_string(),
            ));
        }

        let mut results: Vec<Value> = Vec::new();
        let state_snapshot = self.state_service.dump(Some("any"))?;
        let mut context = serde_json::json!({
            "input": input,
            "state": state_snapshot.get("state").cloned().unwrap_or(Value::Object(Default::default())),
            "steps": {},
            "trace_id": trace_id,
            "span_id": span_id,
            "parent_span_id": parent_span_id,
            "apply": apply,
            "confirm": confirm,
        });

        for (index, step) in steps.iter().enumerate() {
            let step_key = step
                .get("id")
                .or_else(|| step.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or(&format!("step_{}", index + 1))
                .to_string();

            match self
                .execute_step(&tool_executor, step, &step_key, &context, template_missing)
                .await
            {
                Ok(outcome) => {
                    if let Some(obj) = context.get_mut("steps").and_then(|v| v.as_object_mut()) {
                        obj.insert(
                            step_key.clone(),
                            outcome.get("result").cloned().unwrap_or(Value::Null),
                        );
                    }
                    results.push(outcome);
                }
                Err(err) => {
                    let entry = serde_json::json!({
                        "id": step_key,
                        "tool": step.get("tool").cloned().unwrap_or(Value::Null),
                        "action": step.get("args").and_then(|v| v.get("action")).cloned().unwrap_or(Value::Null),
                        "success": false,
                        "error": err.message,
                    });
                    results.push(entry);
                    if stop_on_error
                        && !step
                            .get("continue_on_error")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    {
                        return Ok(serde_json::json!({
                            "success": false,
                            "runbook": runbook.get("name").cloned().unwrap_or(Value::Null),
                            "runbook_manifest": manifest_ref(&runbook),
                            "effects": effects.to_value(),
                            "steps": results,
                            "error": err.message,
                            "trace_id": trace_id,
                        }));
                    }
                }
            }

            let refreshed = self.state_service.dump(Some("any"))?;
            if let Some(state) = refreshed.get("state") {
                if let Some(obj) = context.as_object_mut() {
                    obj.insert("state".to_string(), state.clone());
                }
            }
        }

        Ok(serde_json::json!({
            "success": results.iter().all(|item| item.get("success").and_then(|v| v.as_bool()).unwrap_or(true)),
            "runbook": runbook.get("name").cloned().unwrap_or(Value::Null),
            "runbook_manifest": manifest_ref(&runbook),
            "effects": effects.to_value(),
            "steps": results,
            "trace_id": trace_id,
        }))
    }

    fn evaluate_when(condition: Option<&Value>, context: &Value, missing: &str) -> bool {
        let Some(condition) = condition else {
            return true;
        };
        if condition.is_null() {
            return true;
        }
        if let Some(flag) = condition.as_bool() {
            return flag;
        }
        if let Some(text) = condition.as_str() {
            let resolved = resolve_template_string(text, context, missing).unwrap_or(Value::Null);
            return !resolved.is_null() && resolved != Value::Bool(false);
        }
        let Some(obj) = condition.as_object() else {
            return false;
        };

        if let Some(and_list) = obj.get("and").and_then(|v| v.as_array()) {
            return and_list
                .iter()
                .all(|entry| Self::evaluate_when(Some(entry), context, missing));
        }
        if let Some(or_list) = obj.get("or").and_then(|v| v.as_array()) {
            return or_list
                .iter()
                .any(|entry| Self::evaluate_when(Some(entry), context, missing));
        }
        if let Some(not_val) = obj.get("not") {
            return !Self::evaluate_when(Some(not_val), context, missing);
        }

        let path = obj.get("path").and_then(|v| v.as_str()).map(|p| {
            resolve_template_string(p, context, missing).unwrap_or(Value::String(p.to_string()))
        });
        let value = if let Some(Value::String(path)) = path {
            get_path_value(context, &path, false, Some(Value::Null)).unwrap_or(Value::Null)
        } else {
            obj.get("value").cloned().unwrap_or(Value::Null)
        };

        if let Some(exists) = obj.get("exists").and_then(|v| v.as_bool()) {
            return if exists {
                !value.is_null()
            } else {
                value.is_null()
            };
        }
        if let Some(expected) = obj.get("equals") {
            let expected = resolve_templates(expected, context, missing).unwrap_or(Value::Null);
            return value == expected;
        }
        if let Some(expected) = obj.get("not_equals") {
            let expected = resolve_templates(expected, context, missing).unwrap_or(Value::Null);
            return value != expected;
        }
        if let Some(list) = obj.get("in") {
            let expected = resolve_templates(list, context, missing).unwrap_or(Value::Null);
            if let Some(arr) = expected.as_array() {
                return arr.contains(&value);
            }
            return false;
        }
        if let Some(expected) = obj.get("contains") {
            let expected = resolve_templates(expected, context, missing).unwrap_or(Value::Null);
            if let Some(text) = value.as_str() {
                return text.contains(expected.as_str().unwrap_or(""));
            }
            if let Some(arr) = value.as_array() {
                return arr.contains(&expected);
            }
            return false;
        }
        if let Some(expected) = obj.get("gt") {
            let expected = resolve_templates(expected, context, missing).unwrap_or(Value::Null);
            return value.as_f64().unwrap_or(0.0) > expected.as_f64().unwrap_or(0.0);
        }
        if let Some(expected) = obj.get("gte") {
            let expected = resolve_templates(expected, context, missing).unwrap_or(Value::Null);
            return value.as_f64().unwrap_or(0.0) >= expected.as_f64().unwrap_or(0.0);
        }
        if let Some(expected) = obj.get("lt") {
            let expected = resolve_templates(expected, context, missing).unwrap_or(Value::Null);
            return value.as_f64().unwrap_or(0.0) < expected.as_f64().unwrap_or(0.0);
        }
        if let Some(expected) = obj.get("lte") {
            let expected = resolve_templates(expected, context, missing).unwrap_or(Value::Null);
            return value.as_f64().unwrap_or(0.0) <= expected.as_f64().unwrap_or(0.0);
        }

        !value.is_null() && value != Value::Bool(false)
    }

    async fn execute_step(
        &self,
        tool_executor: &ToolExecutor,
        step: &Value,
        step_key: &str,
        context: &Value,
        missing: &str,
    ) -> Result<Value, ToolError> {
        let tool = step.get("tool").and_then(|v| v.as_str()).ok_or_else(|| {
            ToolError::invalid_params(format!("runbook step '{}' missing tool", step_key))
        })?;
        if tool == "mcp_runbook" {
            return Err(ToolError::denied(
                "Nested runbook execution is not supported",
            ));
        }
        let should_run = Self::evaluate_when(step.get("when"), context, missing);
        if !should_run {
            return Ok(serde_json::json!({
                "id": step_key,
                "tool": tool,
                "action": step.get("args").and_then(|v| v.get("action")).cloned().unwrap_or(Value::Null),
                "skipped": true,
                "success": true,
            }));
        }

        let base_args = step
            .get("args")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let runbook_apply = context
            .get("apply")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let runbook_confirm = context
            .get("confirm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if let Some(foreach) = step.get("foreach") {
            let foreach_config = resolve_templates(foreach, context, missing)?;
            let items = foreach_config
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut results = Vec::new();
            for (idx, item) in items.iter().enumerate() {
                let mut item_context = context.clone();
                if let Some(obj) = item_context.as_object_mut() {
                    obj.insert("item".to_string(), item.clone());
                    obj.insert(
                        "index".to_string(),
                        Value::Number(serde_json::Number::from(idx as i64)),
                    );
                }
                let mut args_for_item = resolve_templates(&base_args, &item_context, missing)?;
                if let Some(obj) = args_for_item.as_object_mut() {
                    if !obj.contains_key("apply") {
                        obj.insert("apply".to_string(), Value::Bool(runbook_apply));
                    }
                    if !obj.contains_key("confirm") {
                        obj.insert("confirm".to_string(), Value::Bool(runbook_confirm));
                    }
                }
                let output = tool_executor.execute(tool, args_for_item).await?;
                results.push(output.get("result").cloned().unwrap_or(output));
            }
            return Ok(serde_json::json!({
                "id": step_key,
                "tool": tool,
                "action": base_args.get("action").cloned().unwrap_or(Value::Null),
                "success": true,
                "result": results,
                "foreach": {"count": items.len()},
            }));
        }

        let mut resolved_args = resolve_templates(&base_args, context, missing)?;
        if let Some(obj) = resolved_args.as_object_mut() {
            if !obj.contains_key("apply") {
                obj.insert("apply".to_string(), Value::Bool(runbook_apply));
            }
            if !obj.contains_key("confirm") {
                obj.insert("confirm".to_string(), Value::Bool(runbook_confirm));
            }
        }
        let output = tool_executor.execute(tool, resolved_args).await?;
        Ok(serde_json::json!({
            "id": step_key,
            "tool": tool,
            "action": base_args.get("action").cloned().unwrap_or(Value::Null),
            "success": true,
            "result": output.get("result").cloned().unwrap_or(output.clone()),
            "meta": output.get("meta").cloned().unwrap_or(Value::Null),
        }))
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for RunbookManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
