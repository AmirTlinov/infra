use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::state::StateService;
use chrono::{Datelike, Timelike};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::Arc;

const DEFAULT_LOCK_TTL_MS: i64 = 15 * 60_000;
const MAX_LOCK_TTL_MS: i64 = 24 * 60 * 60_000;

#[derive(Clone, Debug)]
struct ChangeWindow {
    days: Option<Vec<u32>>,
    start: u32,
    end: u32,
}

#[derive(Clone, Debug)]
struct NormalizedPolicy {
    mode: Option<String>,
    allow_intents: Option<Vec<String>>,
    allow_merge: Option<bool>,
    repo_allowed_remotes: Option<Vec<String>>,
    kubernetes_allowed_namespaces: Option<Vec<String>>,
    change_windows: Option<Vec<ChangeWindow>>,
    lock_enabled: bool,
    lock_ttl_ms: i64,
}

#[derive(Clone)]
pub struct PolicyGuard {
    pub policy: Value,
    pub lock_key: Option<String>,
    state_service: Option<Arc<StateService>>,
    trace_id: String,
}

#[derive(Clone, Copy)]
pub(crate) struct GitopsWriteScope<'a> {
    pub trace_id: &'a str,
    pub project_name: Option<&'a str>,
    pub target_name: Option<&'a str>,
    pub repo_root: Option<&'a str>,
    pub project_context: Option<&'a Value>,
}

impl PolicyGuard {
    pub fn release(&self) -> Result<(), ToolError> {
        let Some(state_service) = self.state_service.as_ref() else {
            return Ok(());
        };
        let Some(key) = self.lock_key.as_deref() else {
            return Ok(());
        };
        let existing = state_service.get(key, Some("persistent"))?;
        let lock = existing.get("value").cloned().unwrap_or(Value::Null);
        let Some(obj) = lock.as_object() else {
            return Ok(());
        };
        if obj.get("trace_id").and_then(|v| v.as_str()) != Some(self.trace_id.as_str()) {
            return Ok(());
        }
        let count = obj
            .get("count")
            .and_then(|v| v.as_i64())
            .unwrap_or(1)
            .max(1);
        if count > 1 {
            let mut next = obj.clone();
            next.insert("count".to_string(), Value::Number((count - 1).into()));
            next.insert(
                "updated_at".to_string(),
                Value::String(chrono::Utc::now().to_rfc3339()),
            );
            let _ = state_service.set(key, Value::Object(next), Some("persistent"));
            return Ok(());
        }
        let _ = state_service.unset(key, Some("persistent"));
        Ok(())
    }
}

#[derive(Clone)]
pub struct PolicyService {
    logger: Logger,
    state_service: Option<Arc<StateService>>,
}

impl PolicyService {
    pub fn new(logger: Logger, state_service: Option<Arc<StateService>>) -> Self {
        Self {
            logger: logger.child("policy"),
            state_service,
        }
    }

    pub fn evaluate_repo_policy(&self, _args: &Value) -> Result<Value, ToolError> {
        Ok(serde_json::json!({"success": true}))
    }

    pub fn resolve_policy(
        &self,
        inputs: Option<&Value>,
        project_context: Option<&Value>,
    ) -> Result<Option<Value>, ToolError> {
        let direct = self.resolve_policy_value(
            inputs.and_then(|v| {
                v.get("policy")
                    .or_else(|| v.get("policy_profile"))
                    .or_else(|| v.get("policy_profile_name"))
            }),
            "inputs.policy",
            project_context,
        )?;
        if direct.is_some() {
            return Ok(direct);
        }

        let from_target = self.resolve_policy_value(
            inputs
                .and_then(|v| v.get("target"))
                .and_then(|v| v.get("policy")),
            "target.policy",
            project_context,
        )?;
        if from_target.is_some() {
            return Ok(from_target);
        }

        let from_context = self.resolve_policy_value(
            project_context
                .and_then(|v| v.get("target"))
                .and_then(|v| v.get("policy")),
            "target.policy",
            project_context,
        )?;
        if from_context.is_some() {
            return Ok(from_context);
        }

        Ok(self.resolve_autonomy_policy())
    }

    fn resolve_autonomy_policy(&self) -> Option<Value> {
        let raw = std::env::var("INFRA_AUTONOMY_POLICY").ok();
        if let Some(raw) = raw {
            let trimmed = raw.trim();
            if trimmed == "operatorless" {
                return Some(serde_json::json!({"mode": "operatorless"}));
            }
            if trimmed.starts_with('{') {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    if parsed.is_object() {
                        return Some(parsed);
                    }
                }
            }
        }

        let autonomy = std::env::var("INFRA_AUTONOMY").ok();
        if autonomy.as_deref().map(read_truth_env).unwrap_or(false) {
            return Some(serde_json::json!({"mode": "operatorless"}));
        }
        None
    }

    fn resolve_policy_profile(&self, name: &str, project_context: Option<&Value>) -> Option<Value> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return None;
        }
        let profiles = project_context
            .and_then(|v| v.get("project"))
            .and_then(|v| v.get("policy_profiles"))
            .and_then(|v| v.as_object());
        let profiles = profiles?;
        profiles.get(trimmed).cloned()
    }

    fn resolve_policy_value(
        &self,
        value: Option<&Value>,
        label: &str,
        project_context: Option<&Value>,
    ) -> Result<Option<Value>, ToolError> {
        let Some(value) = value else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        if let Some(name) = value.as_str() {
            let profile = self.resolve_policy_profile(name, project_context);
            return profile.ok_or_else(|| {
                ToolError::not_found(format!("{} profile '{}' not found", label, name))
                    .with_hint("Use a known project.policy_profiles key, or pass the policy object directly.".to_string())
                    .with_details(serde_json::json!({"profile": name}))
            }).map(Some);
        }
        if value.is_object() {
            return Ok(Some(value.clone()));
        }
        Err(ToolError::invalid_params(format!(
            "{} must be an object or profile name",
            label
        )))
    }

    pub(crate) fn guard_gitops_write(
        &self,
        intent_type: &str,
        inputs: &Value,
        scope: GitopsWriteScope<'_>,
    ) -> Result<PolicyGuard, ToolError> {
        self.logger.debug("guard_gitops_write", None);
        let raw_policy = self.resolve_policy(Some(inputs), scope.project_context)?;
        let Some(raw_policy) = raw_policy else {
            return Err(ToolError::denied("GitOps write intents require policy").with_hint(
                "Provide inputs.policy (mode=operatorless), configure target.policy, set project.policy_profiles, or set INFRA_AUTONOMY_POLICY=operatorless.".to_string(),
            ));
        };
        let normalized = self.normalize_policy(&raw_policy)?;
        self.assert_gitops_write_allowed(intent_type, inputs, &normalized)?;

        let lock_key = if normalized.lock_enabled {
            self.build_lock_key(scope.project_name, scope.target_name, scope.repo_root)
        } else {
            None
        };
        if normalized.lock_enabled && lock_key.is_none() {
            return Err(ToolError::invalid_params(
                "policy.lock.enabled requires project/target or repo_root for lock scope",
            )
            .with_hint(
                "Provide project+target (via workspace/project) or pass repo_root so the lock scope can be derived.".to_string(),
            ));
        }

        if let Some(key) = lock_key.as_deref() {
            self.acquire_lock(
                key,
                scope.trace_id,
                normalized.lock_ttl_ms,
                serde_json::json!({
                    "intent": intent_type,
                    "project": scope.project_name,
                    "target": scope.target_name,
                    "repo_root": scope.repo_root,
                }),
            )?;
        }

        Ok(PolicyGuard {
            policy: normalized.to_value(),
            lock_key,
            state_service: self.state_service.clone(),
            trace_id: scope.trace_id.to_string(),
        })
    }

    pub fn guard_repo_write(
        &self,
        action: &str,
        inputs: &Value,
        trace_id: &str,
        project_context: Option<&Value>,
        repo_root: Option<&str>,
    ) -> Result<Option<PolicyGuard>, ToolError> {
        let raw_policy = self.resolve_policy(Some(inputs), project_context)?;
        let Some(raw_policy) = raw_policy else {
            return Ok(None);
        };
        let normalized = self.normalize_policy(&raw_policy)?;
        self.assert_repo_write_allowed(action, inputs, &normalized)?;

        let lock_key = if normalized.lock_enabled {
            self.build_lock_key(
                project_context
                    .and_then(|v| v.get("projectName"))
                    .and_then(|v| v.as_str()),
                project_context
                    .and_then(|v| v.get("targetName"))
                    .and_then(|v| v.as_str()),
                repo_root,
            )
        } else {
            None
        };

        if normalized.lock_enabled && lock_key.is_none() {
            return Err(ToolError::invalid_params(
                "policy.lock.enabled requires project/target or repo_root for lock scope",
            )
            .with_hint(
                "Provide project+target (via workspace/project) or pass repo_root so the lock scope can be derived.".to_string(),
            ));
        }

        if let Some(key) = lock_key.as_deref() {
            self.acquire_lock(
                key,
                trace_id,
                normalized.lock_ttl_ms,
                serde_json::json!({
                    "action": action,
                    "project": project_context.and_then(|v| v.get("projectName")).and_then(|v| v.as_str()),
                    "target": project_context.and_then(|v| v.get("targetName")).and_then(|v| v.as_str()),
                    "repo_root": repo_root,
                }),
            )?;
        }

        Ok(Some(PolicyGuard {
            policy: normalized.to_value(),
            lock_key,
            state_service: self.state_service.clone(),
            trace_id: trace_id.to_string(),
        }))
    }

    pub fn guard_kubectl_write(
        &self,
        inputs: &Value,
        trace_id: &str,
        project_context: Option<&Value>,
        repo_root: Option<&str>,
    ) -> Result<Option<PolicyGuard>, ToolError> {
        let raw_policy = self.resolve_policy(Some(inputs), project_context)?;
        let Some(raw_policy) = raw_policy else {
            return Ok(None);
        };
        let normalized = self.normalize_policy(&raw_policy)?;
        self.assert_kubectl_write_allowed(inputs, &normalized)?;

        let lock_key = if normalized.lock_enabled {
            self.build_lock_key(
                project_context
                    .and_then(|v| v.get("projectName"))
                    .and_then(|v| v.as_str()),
                project_context
                    .and_then(|v| v.get("targetName"))
                    .and_then(|v| v.as_str()),
                repo_root,
            )
        } else {
            None
        };

        if normalized.lock_enabled && lock_key.is_none() {
            return Err(ToolError::invalid_params(
                "policy.lock.enabled requires project/target or repo_root for lock scope",
            )
            .with_hint(
                "Provide project+target (via workspace/project) or pass repo_root so the lock scope can be derived.".to_string(),
            ));
        }

        if let Some(key) = lock_key.as_deref() {
            self.acquire_lock(
                key,
                trace_id,
                normalized.lock_ttl_ms,
                serde_json::json!({
                    "action": "kubectl",
                    "namespace": inputs.get("namespace").and_then(|v| v.as_str()),
                    "project": project_context.and_then(|v| v.get("projectName")).and_then(|v| v.as_str()),
                    "target": project_context.and_then(|v| v.get("targetName")).and_then(|v| v.as_str()),
                    "repo_root": repo_root,
                }),
            )?;
        }

        Ok(Some(PolicyGuard {
            policy: normalized.to_value(),
            lock_key,
            state_service: self.state_service.clone(),
            trace_id: trace_id.to_string(),
        }))
    }

    fn normalize_policy(&self, policy: &Value) -> Result<NormalizedPolicy, ToolError> {
        let obj = ensure_object(policy, "policy")?;

        let mode = obj
            .get("mode")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let allow_intents = normalize_string_array(
            obj.get("allow").and_then(|v| v.get("intents")),
            "policy.allow.intents",
        )?;
        let allow_merge = obj
            .get("allow")
            .and_then(|v| v.get("merge"))
            .and_then(|v| v.as_bool());

        let repo_allowed_remotes = normalize_string_array(
            obj.get("repo").and_then(|v| v.get("allowed_remotes")),
            "policy.repo.allowed_remotes",
        )?;
        let kubernetes_allowed = normalize_string_array(
            obj.get("kubernetes")
                .and_then(|v| v.get("allowed_namespaces")),
            "policy.kubernetes.allowed_namespaces",
        )?;

        let change_windows = normalize_change_windows(obj.get("change_windows"))?;

        let lock_obj = obj.get("lock").and_then(|v| v.as_object());
        let lock_enabled = lock_obj
            .and_then(|v| v.get("enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let ttl_raw = lock_obj
            .and_then(|v| v.get("ttl_ms"))
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_LOCK_TTL_MS);
        let ttl_ms = ttl_raw.clamp(1, MAX_LOCK_TTL_MS);

        Ok(NormalizedPolicy {
            mode,
            allow_intents,
            allow_merge,
            repo_allowed_remotes,
            kubernetes_allowed_namespaces: kubernetes_allowed,
            change_windows,
            lock_enabled,
            lock_ttl_ms: ttl_ms,
        })
    }

    fn assert_gitops_write_allowed(
        &self,
        intent_type: &str,
        inputs: &Value,
        policy: &NormalizedPolicy,
    ) -> Result<(), ToolError> {
        let Some(mode) = policy.mode.as_deref() else {
            return Err(ToolError::denied("GitOps write intents require policy")
                .with_hint("Provide inputs.policy (mode=operatorless), configure target.policy, or set INFRA_AUTONOMY_POLICY=operatorless.".to_string()));
        };
        if mode != "operatorless" {
            return Err(ToolError::denied(
                "policy.mode=operatorless is required for GitOps write intents",
            )
            .with_hint(
                "Set inputs.policy.mode=\"operatorless\" (or target.policy.mode) and retry."
                    .to_string(),
            ));
        }

        if let Some(intents) = policy.allow_intents.as_ref() {
            if !intents.iter().any(|entry| entry == intent_type) {
                return Err(ToolError::denied(format!("policy denies intent: {}", intent_type))
                    .with_hint("Ask an operator to allow this intent in policy.allow.intents or choose an allowed intent.".to_string())
                    .with_details(serde_json::json!({"intent_type": intent_type})));
            }
        }

        if (intent_type == "gitops.propose" || intent_type == "gitops.release")
            && inputs.get("merge").and_then(|v| v.as_bool()) == Some(true)
            && policy.allow_merge == Some(false)
        {
            return Err(ToolError::denied("policy denies merge").with_hint(
                "Set inputs.merge=false or ask an operator to allow merges (policy.allow.merge)."
                    .to_string(),
            ));
        }

        if let Some(remotes) = policy.repo_allowed_remotes.as_ref() {
            let remote = inputs
                .get("remote")
                .and_then(|v| v.as_str())
                .unwrap_or("origin")
                .trim();
            if !remotes.iter().any(|entry| entry == remote) {
                return Err(ToolError::denied(format!("policy denies git remote: {}", remote))
                    .with_hint("Use an allowed remote or ask an operator to add it to policy.repo.allowed_remotes.".to_string())
                    .with_details(serde_json::json!({"remote": remote})));
            }
        }

        if let Some(namespaces) = policy.kubernetes_allowed_namespaces.as_ref() {
            if let Some(namespace) = inputs.get("namespace").and_then(|v| v.as_str()) {
                let namespace = namespace.trim();
                if !namespaces.iter().any(|entry| entry == namespace) {
                    return Err(ToolError::denied(format!("policy denies namespace: {}", namespace))
                        .with_hint("Choose an allowed namespace or ask an operator to add it to policy.kubernetes.allowed_namespaces.".to_string())
                        .with_details(serde_json::json!({"namespace": namespace})));
                }
            }
        }

        if !is_within_windows_utc(&chrono::Utc::now(), policy.change_windows.as_deref()) {
            return Err(ToolError::denied("policy denies write outside change window")
                .with_hint("Wait for the next change window or ask an operator to adjust policy.change_windows.".to_string()));
        }

        Ok(())
    }

    fn assert_repo_write_allowed(
        &self,
        action: &str,
        inputs: &Value,
        policy: &NormalizedPolicy,
    ) -> Result<(), ToolError> {
        if policy.mode.as_deref() != Some("operatorless") {
            return Err(ToolError::denied(
                "policy.mode=operatorless is required for repo write operations",
            )
            .with_hint(
                "Set policy.mode=\"operatorless\" (in inputs.policy or target.policy) and retry."
                    .to_string(),
            )
            .with_details(serde_json::json!({"action": action})));
        }

        if let Some(remotes) = policy.repo_allowed_remotes.as_ref() {
            let remote = inputs
                .get("remote")
                .and_then(|v| v.as_str())
                .unwrap_or("origin")
                .trim();
            if !remotes.iter().any(|entry| entry == remote) {
                return Err(ToolError::denied(format!("policy denies git remote: {}", remote))
                    .with_hint("Use an allowed remote or ask an operator to add it to policy.repo.allowed_remotes.".to_string())
                    .with_details(serde_json::json!({"remote": remote, "action": action})));
            }
        }

        if !is_within_windows_utc(&chrono::Utc::now(), policy.change_windows.as_deref()) {
            return Err(ToolError::denied("policy denies write outside change window")
                .with_hint("Wait for the next change window or ask an operator to adjust policy.change_windows.".to_string())
                .with_details(serde_json::json!({"action": action})));
        }

        Ok(())
    }

    fn assert_kubectl_write_allowed(
        &self,
        inputs: &Value,
        policy: &NormalizedPolicy,
    ) -> Result<(), ToolError> {
        if policy.mode.as_deref() != Some("operatorless") {
            return Err(ToolError::denied(
                "policy.mode=operatorless is required for kubectl write operations",
            )
            .with_hint(
                "Set policy.mode=\"operatorless\" (in inputs.policy or target.policy) and retry."
                    .to_string(),
            ));
        }

        if let Some(namespaces) = policy.kubernetes_allowed_namespaces.as_ref() {
            let namespace = inputs
                .get("namespace")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if namespace.is_empty() {
                return Err(ToolError::denied("policy requires explicit namespace for kubectl write operations")
                    .with_hint("Pass -n/--namespace (or use a tool that accepts namespace explicitly) and retry.".to_string())
                    .with_details(serde_json::json!({"allowed_namespaces": namespaces})));
            }
            if !namespaces.iter().any(|entry| entry == namespace) {
                return Err(ToolError::denied(format!("policy denies namespace: {}", namespace))
                    .with_hint("Choose an allowed namespace or ask an operator to add it to policy.kubernetes.allowed_namespaces.".to_string())
                    .with_details(serde_json::json!({"namespace": namespace})));
            }
        }

        if !is_within_windows_utc(&chrono::Utc::now(), policy.change_windows.as_deref()) {
            return Err(ToolError::denied("policy denies write outside change window")
                .with_hint("Wait for the next change window or ask an operator to adjust policy.change_windows.".to_string()));
        }
        Ok(())
    }

    fn build_lock_key(
        &self,
        project_name: Option<&str>,
        target_name: Option<&str>,
        repo_root: Option<&str>,
    ) -> Option<String> {
        if let (Some(project), Some(target)) = (project_name, target_name) {
            return Some(format!("gitops.lock.project:{}:{}", project, target));
        }
        repo_root.map(|root| format!("gitops.lock.{}", compute_repo_root_key(root)))
    }

    fn acquire_lock(
        &self,
        key: &str,
        trace_id: &str,
        ttl_ms: i64,
        meta: Value,
    ) -> Result<(), ToolError> {
        let Some(state_service) = self.state_service.as_ref() else {
            return Err(
                ToolError::internal("state service is not available for lock enforcement")
                    .with_hint("Enable StateService in bootstrap.".to_string()),
            );
        };

        let now = chrono::Utc::now();
        let expires = now + chrono::Duration::milliseconds(ttl_ms);
        let now_iso = now.to_rfc3339();
        let expires_iso = expires.to_rfc3339();

        let existing = state_service.get(key, Some("persistent"))?;
        let lock = existing.get("value").cloned().unwrap_or(Value::Null);

        let expired = |current: &Value| -> bool {
            let Some(obj) = current.as_object() else {
                return true;
            };
            let expires_at = obj.get("expires_at").and_then(|v| v.as_str());
            let Some(expires_at) = expires_at else {
                return true;
            };
            let parsed = chrono::DateTime::parse_from_rfc3339(expires_at).ok();
            parsed
                .map(|dt| dt.timestamp_millis() <= now.timestamp_millis())
                .unwrap_or(true)
        };

        if lock.is_object() && !expired(&lock) {
            if lock.get("trace_id").and_then(|v| v.as_str()) == Some(trace_id) {
                let count = lock
                    .get("count")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(1)
                    .max(1);
                let mut next = lock.as_object().cloned().unwrap_or_default();
                next.insert("count".to_string(), Value::Number((count + 1).into()));
                next.insert("updated_at".to_string(), Value::String(now_iso.clone()));
                next.insert("expires_at".to_string(), Value::String(expires_iso.clone()));
                let _ = state_service.set(key, Value::Object(next), Some("persistent"));
                return Ok(());
            }

            return Err(ToolError::conflict(format!(
                "environment lock is held (key={}) until {}",
                key,
                lock.get("expires_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
            ))
            .with_hint(
                "Wait for the lock to expire, or cancel the conflicting operation before retrying."
                    .to_string(),
            )
            .with_details(serde_json::json!({
                "key": key,
                "expires_at": lock.get("expires_at").cloned().unwrap_or(Value::Null),
                "holder_trace_id": lock.get("trace_id").cloned().unwrap_or(Value::Null),
            })));
        }

        let mut next = meta.as_object().cloned().unwrap_or_default();
        next.insert("trace_id".to_string(), Value::String(trace_id.to_string()));
        next.insert("acquired_at".to_string(), Value::String(now_iso.clone()));
        next.insert("updated_at".to_string(), Value::String(now_iso));
        next.insert("expires_at".to_string(), Value::String(expires_iso));
        next.insert("ttl_ms".to_string(), Value::Number(ttl_ms.into()));
        next.insert("count".to_string(), Value::Number(1.into()));
        let _ = state_service.set(key, Value::Object(next), Some("persistent"));
        Ok(())
    }
}

fn ensure_object<'a>(
    value: &'a Value,
    label: &str,
) -> Result<&'a serde_json::Map<String, Value>, ToolError> {
    value
        .as_object()
        .ok_or_else(|| ToolError::invalid_params(format!("{} must be an object", label)))
}

fn normalize_string_array(
    value: Option<&Value>,
    label: &str,
) -> Result<Option<Vec<String>>, ToolError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let arr = value.as_array().ok_or_else(|| {
        ToolError::invalid_params(format!("{} must be an array of strings", label))
    })?;
    let mut out = Vec::new();
    for entry in arr {
        let trimmed = entry
            .as_str()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| entry.to_string());
        if !trimmed.trim().is_empty() {
            out.push(trimmed);
        }
    }
    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

fn read_truth_env(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn parse_time_minutes(raw: &str, label: &str) -> Result<u32, ToolError> {
    if raw == "24:00" {
        return Ok(24 * 60);
    }
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() != 2 {
        return Err(ToolError::invalid_params(format!(
            "{} must be HH:MM (24h)",
            label
        )));
    }
    let hours = parts[0]
        .parse::<u32>()
        .map_err(|_| ToolError::invalid_params(format!("{} hours must be 0-23", label)))?;
    let minutes = parts[1]
        .parse::<u32>()
        .map_err(|_| ToolError::invalid_params(format!("{} minutes must be 0-59", label)))?;
    if hours > 23 {
        return Err(ToolError::invalid_params(format!(
            "{} hours must be 0-23",
            label
        )));
    }
    if minutes > 59 {
        return Err(ToolError::invalid_params(format!(
            "{} minutes must be 0-59",
            label
        )));
    }
    Ok(hours * 60 + minutes)
}

fn normalize_days(value: &Value, label: &str) -> Result<Option<Vec<u32>>, ToolError> {
    let arr = value
        .as_array()
        .ok_or_else(|| ToolError::invalid_params(format!("{} must be an array", label)))?;
    let mut set = HashSet::new();
    for raw in arr {
        if raw == "*" || raw == "all" {
            return Ok(None);
        }
        if let Some(n) = raw.as_u64() {
            if n > 6 {
                return Err(ToolError::invalid_params(format!(
                    "{} entries must be 0-6",
                    label
                )));
            }
            set.insert(n as u32);
            continue;
        }
        let text = raw.as_str().unwrap_or("").trim().to_lowercase();
        if text.is_empty() {
            continue;
        }
        let idx = match &text[..std::cmp::min(3, text.len())] {
            "sun" => 0,
            "mon" => 1,
            "tue" => 2,
            "wed" => 3,
            "thu" => 4,
            "fri" => 5,
            "sat" => 6,
            _ => {
                return Err(ToolError::invalid_params(format!("Unknown day: {}", text)));
            }
        };
        set.insert(idx);
    }
    if set.is_empty() {
        Ok(None)
    } else {
        Ok(Some(set.into_iter().collect()))
    }
}

fn normalize_change_windows(value: Option<&Value>) -> Result<Option<Vec<ChangeWindow>>, ToolError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let arr = value
        .as_array()
        .ok_or_else(|| ToolError::invalid_params("policy.change_windows must be an array"))?;
    let mut windows = Vec::new();
    for entry in arr {
        let obj = ensure_object(entry, "change_windows entry")?;
        let start = obj.get("start").and_then(|v| v.as_str()).unwrap_or("00:00");
        let end = obj.get("end").and_then(|v| v.as_str()).unwrap_or("24:00");
        let days = obj
            .get("days")
            .map(|v| normalize_days(v, "change_windows.days"))
            .transpose()?
            .flatten();
        let tz = obj.get("tz").and_then(|v| v.as_str()).unwrap_or("UTC");
        if tz != "UTC" {
            return Err(
                ToolError::invalid_params("change_windows.tz currently only supports UTC")
                    .with_hint("Omit tz or set tz=\"UTC\".".to_string()),
            );
        }
        windows.push(ChangeWindow {
            days,
            start: parse_time_minutes(start, "change_windows.start")?,
            end: parse_time_minutes(end, "change_windows.end")?,
        });
    }
    if windows.is_empty() {
        Ok(None)
    } else {
        Ok(Some(windows))
    }
}

fn is_within_windows_utc(
    now: &chrono::DateTime<chrono::Utc>,
    windows: Option<&[ChangeWindow]>,
) -> bool {
    let Some(windows) = windows else {
        return true;
    };
    if windows.is_empty() {
        return false;
    }
    let day = now.weekday().num_days_from_sunday();
    let minutes = now.hour() * 60 + now.minute();

    for window in windows {
        let day_allowed = window
            .days
            .as_ref()
            .map(|d| d.contains(&day))
            .unwrap_or(true);
        let prev_day_allowed = window
            .days
            .as_ref()
            .map(|d| d.contains(&((day + 6) % 7)))
            .unwrap_or(true);

        if window.start <= window.end {
            if day_allowed && minutes >= window.start && minutes < window.end {
                return true;
            }
            continue;
        }

        if (day_allowed && minutes >= window.start) || (prev_day_allowed && minutes < window.end) {
            return true;
        }
    }

    false
}

fn compute_repo_root_key(repo_root: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(repo_root.as_bytes());
    let hash = hex::encode(hasher.finalize());
    format!("repo:{}", &hash[..16])
}

impl NormalizedPolicy {
    fn to_value(&self) -> Value {
        serde_json::json!({
            "mode": self.mode,
            "allow": {
                "intents": self.allow_intents,
                "merge": self.allow_merge,
            },
            "repo": {
                "allowed_remotes": self.repo_allowed_remotes,
            },
            "kubernetes": {
                "allowed_namespaces": self.kubernetes_allowed_namespaces,
            },
            "change_windows": self.change_windows.as_ref().map(|windows| {
                windows.iter().map(|window| {
                    serde_json::json!({
                        "days": window.days,
                        "start": format!("{:02}:{:02}", window.start / 60, window.start % 60),
                        "end": format!("{:02}:{:02}", window.end / 60, window.end % 60),
                        "tz": "UTC",
                    })
                }).collect::<Vec<_>>()
            }),
            "lock": {
                "enabled": self.lock_enabled,
                "ttl_ms": self.lock_ttl_ms,
            },
        })
    }
}
