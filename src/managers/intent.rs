use crate::errors::ToolError;
use crate::services::capability::CapabilityService;
use crate::services::context::ContextService;
use crate::services::evidence::EvidenceService;
use crate::services::logger::Logger;
use crate::services::policy::{GitopsWriteScope, PolicyGuard, PolicyService};
use crate::services::project_resolver::ProjectResolver;
use crate::services::runbook::RunbookService;
use crate::services::security::Security;
use crate::services::state::StateService;
use crate::services::tool_executor::{ToolExecutor, ToolHandler};
use crate::services::validation::Validation;
use crate::utils::tool_errors::unknown_action_error;
use crate::utils::when_matcher::matches_when;
use once_cell::sync::OnceCell;
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, Weak};

const INTENT_ACTIONS: &[&str] = &["compile", "dry_run", "execute", "explain"];

#[derive(Clone)]
pub struct IntentManager {
    logger: Logger,
    security: Arc<Security>,
    validation: Validation,
    capability_service: Arc<CapabilityService>,
    runbook_service: Arc<RunbookService>,
    evidence_service: Arc<EvidenceService>,
    state_service: Arc<StateService>,
    project_resolver: Option<Arc<ProjectResolver>>,
    context_service: Option<Arc<ContextService>>,
    policy_service: Option<Arc<PolicyService>>,
    tool_executor: Arc<OnceCell<Weak<ToolExecutor>>>,
}

#[derive(Clone)]
struct NormalizedIntent {
    intent_type: String,
    inputs: serde_json::Map<String, Value>,
    apply: bool,
    project: Option<String>,
    target: Option<String>,
    context: Option<Value>,
    project_context: Option<Value>,
}

impl IntentManager {
    pub fn new(
        logger: Logger,
        security: Arc<Security>,
        validation: Validation,
        capability_service: Arc<CapabilityService>,
        runbook_service: Arc<RunbookService>,
        evidence_service: Arc<EvidenceService>,
        state_service: Arc<StateService>,
        project_resolver: Option<Arc<ProjectResolver>>,
        context_service: Option<Arc<ContextService>>,
        policy_service: Option<Arc<PolicyService>>,
    ) -> Self {
        Self {
            logger: logger.child("intent"),
            security,
            validation,
            capability_service,
            runbook_service,
            evidence_service,
            state_service,
            project_resolver,
            context_service,
            policy_service,
            tool_executor: Arc::new(OnceCell::new()),
        }
    }

    pub fn set_tool_executor(&self, tool_executor: Arc<ToolExecutor>) {
        let _ = self.tool_executor.set(Arc::downgrade(&tool_executor));
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "compile" => self.compile(&args).await,
            "dry_run" => self.execute(&args, true).await,
            "execute" => self.execute(&args, false).await,
            "explain" => self.explain(&args).await,
            _ => Err(unknown_action_error(
                "intent",
                args.get("action"),
                INTENT_ACTIONS,
            )),
        }
    }

    async fn compile(&self, args: &Value) -> Result<Value, ToolError> {
        let (plan, missing) = self.build_plan(args, true).await?;
        Ok(serde_json::json!({"success": true, "plan": plan, "missing": missing}))
    }

    async fn explain(&self, args: &Value) -> Result<Value, ToolError> {
        let intent = self.normalize_intent(args).await?;
        let capability = self
            .resolve_capability(
                &intent.intent_type,
                intent.context.as_ref().or(intent.inputs.get("context")),
            )
            .await?;
        let (resolved_inputs, missing) = normalize_inputs(&intent.inputs, &capability);
        Ok(serde_json::json!({
            "success": true,
            "intent": { "type": intent.intent_type, "inputs": redact_value(&Value::Object(intent.inputs.clone())) },
            "capability": capability,
            "inputs": resolved_inputs,
            "missing": missing,
        }))
    }

    async fn execute(&self, args: &Value, dry_run: bool) -> Result<Value, ToolError> {
        let (plan, missing) = self.build_plan(args, false).await?;
        if !missing.is_empty() {
            return Err(ToolError::invalid_params(format!(
                "Missing required inputs: {}",
                missing.join(", ")
            ))
            .with_hint("Provide the missing intent inputs and retry.".to_string())
            .with_details(serde_json::json!({ "missing": missing })));
        }

        if dry_run {
            let preview = plan
                .get("steps")
                .and_then(|v| v.as_array())
                .unwrap_or(&vec![])
                .iter()
                .map(|step| {
                    serde_json::json!({
                        "capability": step.get("capability").cloned().unwrap_or(Value::Null),
                        "runbook": step.get("runbook").cloned().unwrap_or(Value::Null),
                        "inputs": redact_value(step.get("inputs").unwrap_or(&Value::Null)),
                    })
                })
                .collect::<Vec<_>>();
            return Ok(serde_json::json!({
                "success": true,
                "dry_run": true,
                "plan": plan,
                "preview": preview,
                "missing": missing,
            }));
        }

        let trace_id = args
            .get("trace_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let apply = args
            .get("apply")
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| {
                plan.get("intent")
                    .and_then(|v| v.get("apply"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            });

        if plan
            .get("effects")
            .and_then(|v| v.get("requires_apply"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && !apply
        {
            return Err(
                ToolError::denied("Intent requires apply=true for write/mixed effects").with_hint(
                    "Rerun with apply=true if you intend to perform write operations.".to_string(),
                ),
            );
        }

        let intent_type = plan
            .get("intent")
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let mut policy_guard: Option<PolicyGuard> = None;
        if apply
            && plan
                .get("effects")
                .and_then(|v| v.get("requires_apply"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            && intent_type.starts_with("gitops.")
        {
            if let Some(policy_service) = &self.policy_service {
                let project_name = plan
                    .get("intent")
                    .and_then(|v| v.get("project"))
                    .and_then(|v| v.as_str());
                let target_name = plan
                    .get("intent")
                    .and_then(|v| v.get("target"))
                    .and_then(|v| v.as_str());
                let inputs = plan
                    .get("intent")
                    .and_then(|v| v.get("inputs"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let repo_root = plan
                    .get("intent")
                    .and_then(|v| v.get("inputs"))
                    .and_then(|v| v.get("repo_root"))
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        plan.get("intent")
                            .and_then(|v| v.get("inputs"))
                            .and_then(|v| v.get("context"))
                            .and_then(|v| v.get("root"))
                            .and_then(|v| v.as_str())
                    });
                let project_context = plan.get("intent").and_then(|v| v.get("project_context"));

                policy_guard = Some(policy_service.guard_gitops_write(
                    intent_type,
                    &inputs,
                    GitopsWriteScope {
                        trace_id: trace_id.as_str(),
                        project_name,
                        target_name,
                        repo_root,
                        project_context,
                    },
                )?);
            } else {
                return Err(ToolError::internal(
                    "Policy service is not available for GitOps write intents",
                )
                .with_hint("Enable PolicyService or disable GitOps write intents.".to_string()));
            }
        }

        let stop_on_error = args
            .get("stop_on_error")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let mut results = Vec::new();
        let mut success = true;

        let tool_executor = self
            .tool_executor
            .get()
            .and_then(|executor| executor.upgrade())
            .ok_or_else(|| {
                ToolError::internal("Tool executor is not available for intent execution").with_hint(
                    "App wiring bug: IntentManager.set_tool_executor(...) must be called during initialization.".to_string(),
                )
            })?;
        let runbook_manager = crate::managers::runbook::RunbookManager::new(
            self.logger.clone(),
            self.runbook_service.clone(),
            self.state_service.clone(),
        );
        runbook_manager.set_tool_executor(tool_executor.clone());

        let steps = plan
            .get("steps")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for step in steps.iter() {
            let runbook = step.get("runbook").and_then(|v| v.as_str()).unwrap_or("");
            let inputs = step.get("inputs").cloned().unwrap_or(Value::Null);
            self.runbook_service
                .get_runbook(runbook)
                .map_err(|_| ToolError::not_found(format!("Runbook '{}' not found", runbook)))?;

            let outcome = runbook_manager
                .handle_action(serde_json::json!({
                "action": "runbook_run",
                "name": runbook,
                "input": inputs.clone(),
                "stop_on_error": stop_on_error,
                "template_missing": args.get("template_missing").cloned().unwrap_or(Value::String("error".to_string())),
                "trace_id": trace_id,
                "span_id": args.get("span_id").cloned().unwrap_or(Value::Null),
                "parent_span_id": args.get("parent_span_id").cloned().unwrap_or(Value::Null),
            }))
                .await?;

            results.push(serde_json::json!({
                "capability": step.get("capability").cloned().unwrap_or(Value::Null),
                "runbook": runbook,
                "result": outcome,
            }));

            if !outcome
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                success = false;
                if stop_on_error {
                    break;
                }
            }
        }

        if let Some(guard) = policy_guard.as_ref() {
            if let Err(err) = guard.release() {
                self.logger.warn(
                    "Failed to release policy lock",
                    Some(&serde_json::json!({"error": err.message})),
                );
            }
        }

        let evidence = serde_json::json!({
            "intent": redact_value(plan.get("intent").unwrap_or(&Value::Null)),
            "effects": plan.get("effects").cloned().unwrap_or(Value::Null),
            "dry_run": false,
            "executed_at": chrono::Utc::now().to_rfc3339(),
            "steps": results,
            "success": success,
        });

        let mut evidence_path = None;
        if args
            .get("save_evidence")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            if let Ok(saved) = self.evidence_service.save_evidence(&evidence) {
                evidence_path = saved.get("path").cloned();
            }
        }

        Ok(serde_json::json!({
            "success": success,
            "dry_run": false,
            "plan": plan,
            "results": results,
            "evidence": evidence,
            "evidence_path": evidence_path,
        }))
    }

    async fn normalize_intent(&self, args: &Value) -> Result<NormalizedIntent, ToolError> {
        let intent_obj = self
            .validation
            .ensure_object(args.get("intent").unwrap_or(&Value::Null), "Intent")?;
        let intent_type = self.validation.ensure_string(
            intent_obj.get("type").unwrap_or(&Value::Null),
            "Intent type",
            true,
        )?;

        let mut inputs = self
            .validation
            .ensure_optional_object(intent_obj.get("inputs"), "Intent inputs")?
            .unwrap_or_default();

        let apply = args
            .get("apply")
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| {
                intent_obj
                    .get("apply")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            });

        let mut project = self.validation.ensure_optional_string(
            args.get("project").or_else(|| intent_obj.get("project")),
            "Project",
            true,
        )?;
        let mut target = self.validation.ensure_optional_string(
            args.get("target").or_else(|| intent_obj.get("target")),
            "Target",
            true,
        )?;

        let resolve_from_inputs =
            |value: Option<&Value>, label: &str| -> Result<Option<String>, ToolError> {
                match value {
                    None => Ok(None),
                    Some(v) if v.is_null() => Ok(None),
                    Some(v) => {
                        if let Some(text) = v.as_str() {
                            let trimmed = text.trim();
                            if trimmed.is_empty() {
                                return Ok(None);
                            }
                            return Ok(Some(self.validation.ensure_identifier(trimmed, label)?));
                        }
                        let text = v.to_string();
                        if text.trim().is_empty() {
                            return Ok(None);
                        }
                        Ok(Some(self.validation.ensure_identifier(&text, label)?))
                    }
                }
            };

        if project.is_none() {
            project = resolve_from_inputs(inputs.get("project_name"), "Project")?;
        }
        if target.is_none() {
            target = resolve_from_inputs(inputs.get("target_name"), "Target")?;
        }

        let mut context: Option<Value> = None;
        let mut project_context: Option<Value> = None;
        if let Some(resolver) = &self.project_resolver {
            if let Ok(Some(ctx)) = resolver
                .resolve_context(&serde_json::json!({
                    "project": project,
                    "target": target,
                }))
                .await
            {
                project_context = Some(ctx.clone());
                context = Some(ctx.clone());
                if project.is_none() {
                    project = ctx
                        .get("projectName")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
                if target.is_none() {
                    target = ctx
                        .get("targetName")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
                if let Some(project_val) = ctx.get("project") {
                    inputs
                        .entry("project".to_string())
                        .or_insert_with(|| project_val.clone());
                }
                if let Some(target_val) = ctx.get("target") {
                    inputs
                        .entry("target".to_string())
                        .or_insert_with(|| target_val.clone());
                }
            }
        }

        if let Some(project) = project.as_ref() {
            inputs
                .entry("project_name".to_string())
                .or_insert_with(|| Value::String(project.clone()));
        }
        if let Some(target) = target.as_ref() {
            inputs
                .entry("target_name".to_string())
                .or_insert_with(|| Value::String(target.clone()));
        }

        if self.context_service.is_some() && !inputs.contains_key("context") {
            let context_args = serde_json::json!({
                "project": project,
                "target": target,
                "cwd": args.get("cwd").or_else(|| intent_obj.get("cwd")).cloned().unwrap_or(Value::Null),
                "repo_root": args.get("repo_root").or_else(|| intent_obj.get("repo_root")).cloned().unwrap_or(Value::Null),
                "key": args.get("context_key").cloned().unwrap_or(Value::Null),
                "refresh": args.get("context_refresh").and_then(|v| v.as_bool()).unwrap_or(false),
            });
            if let Some(context_service) = &self.context_service {
                match context_service.get_context(&context_args).await {
                    Ok(result) => {
                        if let Some(ctx) = result.get("context") {
                            inputs.insert("context".to_string(), ctx.clone());
                            context = Some(ctx.clone());
                        }
                    }
                    Err(err) => {
                        self.logger.warn(
                            "Context resolution failed",
                            Some(&serde_json::json!({"error": err.message})),
                        );
                    }
                }
            }
        }

        Ok(NormalizedIntent {
            intent_type,
            inputs,
            apply,
            project,
            target,
            context,
            project_context,
        })
    }

    async fn resolve_capability(
        &self,
        intent_type: &str,
        context: Option<&Value>,
    ) -> Result<Value, ToolError> {
        let candidates = self.capability_service.find_all_by_intent(intent_type)?;
        if candidates.is_empty() {
            return Err(ToolError::not_found(format!(
                "Capability for intent '{}' not found",
                intent_type
            ))
            .with_hint(
                "Check capabilities.json (or configure capability mappings) and retry.".to_string(),
            )
            .with_details(serde_json::json!({"intent_type": intent_type})));
        }
        let context_value = context
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let mut matched: Vec<Value> = Vec::new();
        for candidate in candidates {
            let when = candidate.get("when").cloned().unwrap_or(Value::Null);
            if matches_when(&when, &context_value) {
                matched.push(candidate);
            }
        }
        if matched.is_empty() {
            return Err(ToolError::not_found(format!("No capability matched when-clause for intent '{}'", intent_type))
                .with_hint("Provide the required context inputs (project/target/repo_root/etc) or adjust capability.when clauses.".to_string())
                .with_details(serde_json::json!({"intent_type": intent_type})));
        }
        matched.sort_by(|a, b| {
            let a_name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let b_name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let a_direct = if a_name == intent_type { 0 } else { 1 };
            let b_direct = if b_name == intent_type { 0 } else { 1 };
            if a_direct != b_direct {
                return a_direct.cmp(&b_direct);
            }
            a_name.cmp(b_name)
        });
        Ok(matched[0].clone())
    }

    async fn resolve_dependencies(&self, root_name: &str) -> Result<Vec<Value>, ToolError> {
        let mut ordered: Vec<Value> = Vec::new();
        let mut visiting: HashSet<String> = HashSet::new();
        let mut visited: HashSet<String> = HashSet::new();

        fn visit_node(
            service: &CapabilityService,
            name: &str,
            visiting: &mut HashSet<String>,
            visited: &mut HashSet<String>,
            ordered: &mut Vec<Value>,
        ) -> Result<(), ToolError> {
            if visited.contains(name) {
                return Ok(());
            }
            if visiting.contains(name) {
                return Err(ToolError::internal(format!(
                    "Capability dependency cycle at '{}'",
                    name
                ))
                .with_hint("Fix capability.depends_on to remove cycles.".to_string())
                .with_details(serde_json::json!({"capability": name})));
            }

            visiting.insert(name.to_string());
            let capability = service.get_capability(name)?;
            let deps = capability
                .get("depends_on")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for dep in deps {
                if let Some(dep_name) = dep.as_str() {
                    visit_node(service, dep_name, visiting, visited, ordered)?;
                }
            }
            visiting.remove(name);
            visited.insert(name.to_string());
            ordered.push(capability);
            Ok(())
        }

        visit_node(
            self.capability_service.as_ref(),
            root_name,
            &mut visiting,
            &mut visited,
            &mut ordered,
        )?;
        Ok(ordered)
    }

    async fn build_plan(
        &self,
        args: &Value,
        allow_missing: bool,
    ) -> Result<(Value, Vec<String>), ToolError> {
        let intent = self.normalize_intent(args).await?;
        let root = self
            .resolve_capability(
                &intent.intent_type,
                intent.context.as_ref().or(intent.inputs.get("context")),
            )
            .await?;
        let root_name = root
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(&intent.intent_type);
        let ordered = self.resolve_dependencies(root_name).await?;

        let mut steps = Vec::new();
        let mut missing = Vec::new();

        for capability in ordered.iter() {
            let (resolved_inputs, missing_inputs) = normalize_inputs(&intent.inputs, capability);
            let mut resolved_inputs = match resolved_inputs {
                Value::Object(map) => map,
                _ => Default::default(),
            };
            resolved_inputs.insert("apply".to_string(), Value::Bool(intent.apply));

            if let Some(policy_service) = &self.policy_service {
                if let Ok(Some(policy)) = policy_service.resolve_policy(
                    Some(&Value::Object(resolved_inputs.clone())),
                    intent.project_context.as_ref(),
                ) {
                    resolved_inputs.insert("policy".to_string(), policy);
                }
            }

            let effects = capability.get("effects").cloned().unwrap_or(Value::Null);
            steps.push(serde_json::json!({
                "capability": capability.get("name").cloned().unwrap_or(Value::Null),
                "runbook": capability.get("runbook").cloned().unwrap_or(Value::Null),
                "inputs": Value::Object(resolved_inputs.clone()),
                "effects": effects,
            }));

            if !missing_inputs.is_empty() {
                missing.extend(missing_inputs.into_iter().map(|key| {
                    let cap_name = capability
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    format!("{}.{}", cap_name, key)
                }));
            }
        }

        if !allow_missing && !missing.is_empty() {
            return Err(ToolError::invalid_params(format!(
                "Missing required inputs: {}",
                missing.join(", ")
            ))
            .with_hint("Provide the missing intent inputs and retry.".to_string())
            .with_details(serde_json::json!({ "missing": missing })));
        }

        let plan = serde_json::json!({
            "intent": {
                "type": intent.intent_type,
                "inputs": Value::Object(intent.inputs.clone()),
                "apply": intent.apply,
                "project": intent.project,
                "target": intent.target,
                "context": intent.context,
                "project_context": intent.project_context,
            },
            "steps": steps,
            "effects": aggregate_effects(&steps),
        });

        self.security
            .ensure_size_fits(&serde_json::to_string(&plan).unwrap_or_default(), None)?;

        Ok((plan, missing))
    }
}

#[async_trait::async_trait]
impl ToolHandler for IntentManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.handle_action(args).await
    }
}

fn get_by_path(source: &Value, path: &str) -> Option<Value> {
    if path.trim().is_empty() {
        return None;
    }
    let mut current = source;
    for part in path.split('.').filter(|p| !p.is_empty()) {
        let obj = current.as_object()?;
        current = obj.get(part)?;
    }
    Some(current.clone())
}

fn normalize_inputs(
    intent_inputs: &serde_json::Map<String, Value>,
    capability: &Value,
) -> (Value, Vec<String>) {
    let inputs_cfg = capability
        .get("inputs")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let defaults = inputs_cfg
        .get("defaults")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let map_cfg = inputs_cfg
        .get("map")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let pass_through = inputs_cfg
        .get("pass_through")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let mut resolved = defaults.clone();
    let intent_value = Value::Object(intent_inputs.clone());

    for (target, source) in map_cfg {
        if let Some(path) = source.as_str() {
            if let Some(value) = get_by_path(&intent_value, path) {
                resolved.insert(target.clone(), value);
            }
        }
    }

    if pass_through {
        for (key, value) in intent_inputs {
            resolved.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }

    let mut missing = Vec::new();
    let required = inputs_cfg
        .get("required")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for entry in required {
        let key = entry.as_str().unwrap_or("").to_string();
        if key.is_empty() {
            continue;
        }
        let missing_entry = match resolved.get(&key) {
            None => true,
            Some(Value::Null) => true,
            Some(Value::String(text)) => text.trim().is_empty(),
            _ => false,
        };
        if missing_entry {
            missing.push(key);
        }
    }

    (Value::Object(resolved), missing)
}

fn aggregate_effects(steps: &[Value]) -> Value {
    let mut requires_apply = false;
    let mut kind = "read";
    for step in steps {
        let effects = step
            .get("effects")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let effect_kind = effects
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("read");
        let effect_requires = effects
            .get("requires_apply")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if effect_requires || effect_kind == "write" || effect_kind == "mixed" {
            requires_apply = true;
        }
        if effect_kind == "mixed" {
            kind = "mixed";
        } else if effect_kind == "write" && kind != "mixed" {
            kind = "write";
        }
    }
    serde_json::json!({"kind": kind, "requires_apply": requires_apply})
}

fn redact_value(value: &Value) -> Value {
    let re = Regex::new(r"(?i)(key|token|secret|pass|pwd)").unwrap();
    match value {
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, entry) in map {
                if re.is_match(key) {
                    out.insert(key.clone(), Value::String("***".to_string()));
                } else {
                    out.insert(key.clone(), redact_value(entry));
                }
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}
