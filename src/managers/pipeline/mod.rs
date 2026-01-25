mod flows;
mod http;
mod postgres;
mod sftp;
mod util;

use crate::errors::ToolError;
use crate::managers::api::ApiManager;
use crate::managers::postgres::PostgresManager;
use crate::managers::ssh::SshManager;
use crate::services::audit::AuditService;
use crate::services::cache::CacheService;
use crate::services::logger::Logger;
use crate::services::project_resolver::ProjectResolver;
use crate::services::tool_executor::ToolHandler;
use crate::services::validation::Validation;
use crate::utils::redact::redact_object;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

const PIPELINE_ACTIONS: &[&str] = &["run", "describe", "deploy_smoke"];

const PIPELINE_FLOWS: &[&str] = &[
    "http_to_sftp",
    "sftp_to_http",
    "http_to_postgres",
    "sftp_to_postgres",
    "postgres_to_sftp",
    "postgres_to_http",
];

#[derive(Clone, Debug)]
struct Trace {
    trace_id: String,
    parent_span_id: Option<String>,
}

#[derive(Clone)]
pub struct PipelineManager {
    logger: Logger,
    validation: Validation,
    api_manager: Arc<ApiManager>,
    ssh_manager: Arc<SshManager>,
    postgres_manager: Arc<PostgresManager>,
    cache_service: Option<Arc<CacheService>>,
    audit_service: Option<Arc<AuditService>>,
    project_resolver: Option<Arc<ProjectResolver>>,
}

impl PipelineManager {
    pub fn new(
        logger: Logger,
        validation: Validation,
        api_manager: Arc<ApiManager>,
        ssh_manager: Arc<SshManager>,
        postgres_manager: Arc<PostgresManager>,
        cache_service: Option<Arc<CacheService>>,
        audit_service: Option<Arc<AuditService>>,
        project_resolver: Option<Arc<ProjectResolver>>,
    ) -> Self {
        Self {
            logger: logger.child("pipeline"),
            validation,
            api_manager,
            ssh_manager,
            postgres_manager,
            cache_service,
            audit_service,
            project_resolver,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "describe" => Ok(self.describe()),
            "run" => self.run_pipeline(&args).await,
            "deploy_smoke" => self.deploy_smoke(&args).await,
            _ => Err(unknown_action_error("pipeline", action, PIPELINE_ACTIONS)),
        }
    }

    fn describe(&self) -> Value {
        serde_json::json!({
            "success": true,
            "flows": PIPELINE_FLOWS,
        })
    }

    async fn run_pipeline(&self, args: &Value) -> Result<Value, ToolError> {
        let flow = args
            .get("flow")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_lowercase();

        if flow.is_empty() {
            return Err(ToolError::invalid_params("pipeline flow is required")
                .with_hint(format!("Use one of: {}", PIPELINE_FLOWS.join(", "))));
        }

        match flow.as_str() {
            "http_to_sftp" => self.http_to_sftp(args).await,
            "sftp_to_http" => self.sftp_to_http(args).await,
            "http_to_postgres" => self.http_to_postgres(args).await,
            "sftp_to_postgres" => self.sftp_to_postgres(args).await,
            "postgres_to_sftp" => self.postgres_to_sftp(args).await,
            "postgres_to_http" => self.postgres_to_http(args).await,
            _ => Err(
                ToolError::invalid_params(format!("Unknown pipeline flow: {}", flow))
                    .with_hint(format!("Use one of: {}", PIPELINE_FLOWS.join(", "))),
            ),
        }
    }

    fn build_trace(&self, args: &Value) -> Trace {
        let trace_id = args
            .get("trace_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let parent_span_id = args
            .get("span_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                args.get("parent_span_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });

        Trace {
            trace_id,
            parent_span_id,
        }
    }

    fn merge_project_context(&self, child_args: &Value, root_args: &Value) -> Value {
        let Some(obj) = child_args.as_object() else {
            return child_args.clone();
        };

        let mut merged = obj.clone();

        let has_project = merged.contains_key("project") || merged.contains_key("project_name");
        if !has_project {
            let root_project = root_args
                .get("project")
                .or_else(|| root_args.get("project_name"));
            if let Some(root_project) = root_project {
                merged.insert("project".to_string(), root_project.clone());
            }
        }

        let has_target = merged.contains_key("target")
            || merged.contains_key("project_target")
            || merged.contains_key("environment");
        if !has_target {
            let root_target = root_args
                .get("target")
                .or_else(|| root_args.get("project_target"))
                .or_else(|| root_args.get("environment"));
            if let Some(root_target) = root_target {
                merged.insert("target".to_string(), root_target.clone());
            }
        }

        let has_vault =
            merged.contains_key("vault_profile_name") || merged.contains_key("vault_profile");
        if !has_vault {
            if let Some(name) = root_args.get("vault_profile_name") {
                merged.insert("vault_profile_name".to_string(), name.clone());
            } else if let Some(profile) = root_args.get("vault_profile") {
                merged.insert("vault_profile".to_string(), profile.clone());
            }
        }

        Value::Object(merged)
    }

    async fn hydrate_project_defaults(&self, args: &Value) -> Result<Value, ToolError> {
        let Some(project_resolver) = self.project_resolver.as_ref() else {
            return Ok(args.clone());
        };

        let needs_sftp_profile = args
            .get("sftp")
            .and_then(|v| v.as_object())
            .map(|sftp| !sftp.contains_key("profile_name") && !sftp.contains_key("connection"))
            .unwrap_or(false);

        let needs_postgres_profile = args
            .get("postgres")
            .and_then(|v| v.as_object())
            .map(|pg| {
                !pg.contains_key("profile_name")
                    && !pg.contains_key("connection")
                    && !pg.contains_key("connection_url")
            })
            .unwrap_or(false);

        let explicitly_scoped = args.get("project").is_some()
            || args.get("project_name").is_some()
            || args.get("target").is_some()
            || args.get("project_target").is_some()
            || args.get("environment").is_some();

        if !explicitly_scoped && !needs_sftp_profile && !needs_postgres_profile {
            return Ok(args.clone());
        }

        let context = project_resolver.resolve_context(args).await?;
        let Some(context) = context else {
            return Ok(args.clone());
        };
        let target = context.get("target").cloned().unwrap_or(Value::Null);

        let mut hydrated = args.clone();
        let Some(root) = hydrated.as_object_mut() else {
            return Ok(hydrated);
        };

        if let Some(http) = root.get("http").cloned().filter(|v| v.is_object()) {
            let mut http_args = self.merge_project_context(&http, args);
            if http_args.get("profile_name").is_none() {
                if let Some(profile) = target.get("api_profile").cloned() {
                    if let Some(obj) = http_args.as_object_mut() {
                        obj.insert("profile_name".to_string(), profile);
                    }
                }
            }
            root.insert("http".to_string(), http_args);
        }

        if let Some(pg) = root.get("postgres").cloned().filter(|v| v.is_object()) {
            let mut pg_args = self.merge_project_context(&pg, args);
            if pg_args.get("profile_name").is_none() {
                if let Some(profile) = target.get("postgres_profile").cloned() {
                    if let Some(obj) = pg_args.as_object_mut() {
                        obj.insert("profile_name".to_string(), profile);
                    }
                }
            }
            root.insert("postgres".to_string(), pg_args);
        }

        if let Some(sftp) = root.get("sftp").cloned().filter(|v| v.is_object()) {
            let mut sftp_args = self.merge_project_context(&sftp, args);
            if sftp_args.get("profile_name").is_none() {
                if let Some(profile) = target.get("ssh_profile").cloned() {
                    if let Some(obj) = sftp_args.as_object_mut() {
                        obj.insert("profile_name".to_string(), profile);
                    }
                }
            }
            root.insert("sftp".to_string(), sftp_args);
        }

        Ok(hydrated)
    }

    fn audit_stage(&self, stage: &str, trace: &Trace, details: Value, error: Option<&ToolError>) {
        let Some(audit_service) = self.audit_service.as_ref() else {
            return;
        };

        let mut entry = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "status": if error.is_some() { "error" } else { "ok" },
            "tool": "mcp_pipeline",
            "action": stage,
            "trace_id": trace.trace_id,
            "span_id": uuid::Uuid::new_v4().to_string(),
            "parent_span_id": trace.parent_span_id,
            "details": redact_object(&details, 2048, None),
        });

        if let Some(err) = error {
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("error".to_string(), Value::String(err.message.clone()));
            }
        }

        audit_service.append(&entry);
    }

    async fn deploy_smoke(&self, args: &Value) -> Result<Value, ToolError> {
        let started = std::time::Instant::now();
        let trace = self.build_trace(args);

        let local_path = self.validation.ensure_string(
            args.get("local_path").unwrap_or(&Value::Null),
            "local_path",
            true,
        )?;
        let remote_path = self.validation.ensure_string(
            args.get("remote_path").unwrap_or(&Value::Null),
            "remote_path",
            true,
        )?;
        let url =
            self.validation
                .ensure_string(args.get("url").unwrap_or(&Value::Null), "url", true)?;

        let settle_ms = std::cmp::min(
            util::read_positive_int(args.get("settle_ms")).unwrap_or(0) as u64,
            120_000,
        );
        let max_attempts = std::cmp::min(
            util::read_positive_int(args.get("smoke_attempts")).unwrap_or(5) as u64,
            20,
        ) as usize;
        let delay_ms = std::cmp::min(
            util::read_positive_int(args.get("smoke_delay_ms")).unwrap_or(1_000) as u64,
            60_000,
        );
        let smoke_timeout_ms = std::cmp::min(
            util::read_positive_int(args.get("smoke_timeout_ms")).unwrap_or(10_000) as u64,
            120_000,
        );

        self.audit_stage(
            "deploy_smoke.deploy",
            &trace,
            serde_json::json!({"local_path": local_path, "remote_path": remote_path}),
            None,
        );

        let deploy = self
            .ssh_manager
            .handle_action(serde_json::json!({
                "action": "deploy_file",
                "local_path": local_path,
                "remote_path": remote_path,
                "restart": args.get("restart").cloned().unwrap_or(Value::Null),
                "restart_command": args.get("restart_command").cloned().unwrap_or(Value::Null),
                "profile_name": args.get("profile_name").cloned().unwrap_or(Value::Null),
                "connection": args.get("connection").cloned().unwrap_or(Value::Null),
                "project": args.get("project").cloned().unwrap_or(Value::Null),
                "project_name": args.get("project_name").cloned().unwrap_or(Value::Null),
                "target": args.get("target").cloned().unwrap_or(Value::Null),
                "project_target": args.get("project_target").cloned().unwrap_or(Value::Null),
                "environment": args.get("environment").cloned().unwrap_or(Value::Null),
                "vault_profile_name": args.get("vault_profile_name").cloned().unwrap_or(Value::Null),
                "vault_profile": args.get("vault_profile").cloned().unwrap_or(Value::Null),
            }))
            .await?;

        let deploy_ok = deploy
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !deploy_ok {
            self.audit_stage(
                "deploy_smoke.failed",
                &trace,
                serde_json::json!({"stage": "deploy"}),
                None,
            );
            return Ok(serde_json::json!({
                "success": false,
                "code": "DEPLOY_FAILED",
                "deploy": deploy,
                "smoke": Value::Null,
                "duration_ms": started.elapsed().as_millis(),
            }));
        }

        if settle_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(settle_ms)).await;
        }

        self.audit_stage(
            "deploy_smoke.smoke",
            &trace,
            serde_json::json!({"url": url, "attempts": max_attempts}),
            None,
        );

        let mut last: Option<Value> = None;
        let mut ok_at: Option<usize> = None;
        for attempt in 1..=max_attempts {
            let smoke = self
                .api_manager
                .handle_action(serde_json::json!({
                    "action": "smoke_http",
                    "url": url,
                    "timeout_ms": smoke_timeout_ms,
                    "expect_code": args.get("expect_code").cloned().unwrap_or(Value::Null),
                    "follow_redirects": args.get("follow_redirects").cloned().unwrap_or(Value::Null),
                    "insecure_ok": args.get("insecure_ok").cloned().unwrap_or(Value::Null),
                }))
                .await?;
            let ok = smoke
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                && smoke.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            last = Some(smoke);
            if ok {
                ok_at = Some(attempt);
                break;
            }
            if attempt < max_attempts && delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }

        let last = last.unwrap_or(Value::Null);
        let smoke_ok = last
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && last.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        let success = deploy_ok && smoke_ok;

        let summary = if smoke_ok {
            "deploy ok; smoke ok"
        } else {
            "deploy ok; smoke failed"
        };
        let next_actions = if smoke_ok {
            Vec::new()
        } else {
            vec![serde_json::json!({
                "tool": "api",
                "action": "smoke_http",
                "args": {
                    "url": url,
                    "expect_code": args.get("expect_code").cloned().unwrap_or(Value::Number(200.into())),
                    "follow_redirects": args.get("follow_redirects").cloned().unwrap_or(Value::Bool(true)),
                    "insecure_ok": args.get("insecure_ok").cloned().unwrap_or(Value::Bool(true)),
                }
            })]
        };

        Ok(serde_json::json!({
            "success": success,
            "summary": summary,
            "deploy": deploy,
            "smoke": last,
            "attempts": {
                "max_attempts": max_attempts,
                "ok_at": ok_at,
                "delay_ms": delay_ms,
                "timeout_ms": smoke_timeout_ms,
            },
            "next_actions": next_actions,
            "duration_ms": started.elapsed().as_millis(),
        }))
    }
}

#[async_trait::async_trait]
impl ToolHandler for PipelineManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
