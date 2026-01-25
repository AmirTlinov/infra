use crate::errors::ToolError;
use crate::services::job::JobService;
use crate::services::logger::Logger;
use crate::services::validation::Validation;
use crate::utils::tool_errors::unknown_action_error;
use chrono::TimeZone;
use serde_json::Value;
use std::sync::Arc;

const JOB_ACTIONS: &[&str] = &[
    "job_status",
    "job_wait",
    "job_logs_tail",
    "tail_job",
    "follow_job",
    "job_cancel",
    "job_forget",
    "job_list",
];

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn read_positive_int(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    if let Some(n) = value.as_i64() {
        if n > 0 {
            return Some(n as u64);
        }
    }
    if let Some(text) = value.as_str() {
        if let Ok(parsed) = text.parse::<u64>() {
            if parsed > 0 {
                return Some(parsed);
            }
        }
    }
    None
}

fn public_job_view(job: &Value) -> Value {
    if !job.is_object() {
        return Value::Null;
    }
    let expires = job
        .get("expires_at_ms")
        .and_then(|v| v.as_i64())
        .and_then(|ms| {
            chrono::Utc
                .timestamp_millis_opt(ms)
                .single()
                .map(|dt| dt.to_rfc3339())
        });
    serde_json::json!({
        "job_id": job.get("job_id").cloned().unwrap_or(Value::Null),
        "kind": job.get("kind").cloned().unwrap_or(Value::Null),
        "status": job.get("status").cloned().unwrap_or(Value::Null),
        "trace_id": job.get("trace_id").cloned().unwrap_or(Value::Null),
        "parent_span_id": job.get("parent_span_id").cloned().unwrap_or(Value::Null),
        "created_at": job.get("created_at").cloned().unwrap_or(Value::Null),
        "started_at": job.get("started_at").cloned().unwrap_or(Value::Null),
        "updated_at": job.get("updated_at").cloned().unwrap_or(Value::Null),
        "ended_at": job.get("ended_at").cloned().unwrap_or(Value::Null),
        "expires_at": expires,
        "progress": job.get("progress").cloned().unwrap_or(Value::Null),
        "artifacts": job.get("artifacts").cloned().unwrap_or(Value::Null),
        "provider": job.get("provider").cloned().unwrap_or(Value::Null),
        "error": job.get("error").cloned().unwrap_or(Value::Null),
    })
}

#[derive(Clone)]
pub struct JobManager {
    logger: Logger,
    validation: Validation,
    job_service: Arc<JobService>,
    ssh_manager: Option<Arc<crate::managers::ssh::SshManager>>,
}

impl JobManager {
    pub fn new(
        logger: Logger,
        validation: Validation,
        job_service: Arc<JobService>,
        ssh_manager: Option<Arc<crate::managers::ssh::SshManager>>,
    ) -> Self {
        Self {
            logger: logger.child("job"),
            validation,
            job_service,
            ssh_manager,
        }
    }

    fn ensure_job_id(&self, value: &Value) -> Result<String, ToolError> {
        self.validation.ensure_string(value, "job_id", true)
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "job_status" => self.job_status(args).await,
            "job_wait" => self.job_wait(args).await,
            "job_logs_tail" => self.job_logs_tail(args).await,
            "tail_job" => self.tail_job(args).await,
            "follow_job" => self.follow_job(args).await,
            "job_cancel" => self.job_cancel(args).await,
            "job_forget" => self.job_forget(args).await,
            "job_list" => self.job_list(args).await,
            _ => Err(unknown_action_error("job", action, JOB_ACTIONS)),
        }
    }

    async fn job_status(&self, args: Value) -> Result<Value, ToolError> {
        let job_id = self.ensure_job_id(args.get("job_id").unwrap_or(&Value::Null))?;
        let job = self.job_service.get(&job_id);
        let Some(job) = job else {
            return Ok(
                serde_json::json!({"success": false, "code": "NOT_FOUND", "job_id": job_id}),
            );
        };
        if job
            .get("provider")
            .and_then(|v| v.get("tool"))
            .and_then(|v| v.as_str())
            == Some("mcp_ssh_manager")
        {
            if let Some(ssh) = &self.ssh_manager {
                let status = ssh.handle_action(args.clone()).await?;
                if status
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    let exited = status
                        .get("exited")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if exited {
                        let next = if status.get("exit_code").and_then(|v| v.as_i64()) == Some(0) {
                            "succeeded"
                        } else {
                            "failed"
                        };
                        let _ = self.job_service.upsert(serde_json::json!({
                            "job_id": job_id,
                            "status": next,
                            "started_at": job.get("started_at").cloned().unwrap_or(job.get("created_at").cloned().unwrap_or(Value::Null)),
                            "ended_at": job.get("ended_at").cloned().unwrap_or(Value::String(now_iso())),
                        }));
                    }
                }
                return Ok(
                    serde_json::json!({"success": true, "job": public_job_view(self.job_service.get(&job_id).as_ref().unwrap_or(&Value::Null)), "status": status}),
                );
            }
            return Err(ToolError::internal("SSH manager is not available"));
        }
        Ok(
            serde_json::json!({"success": false, "code": "NOT_SUPPORTED", "job_id": job_id, "kind": job.get("kind").cloned().unwrap_or(Value::Null)}),
        )
    }

    async fn job_wait(&self, args: Value) -> Result<Value, ToolError> {
        let job_id = self.ensure_job_id(args.get("job_id").unwrap_or(&Value::Null))?;
        let job = self.job_service.get(&job_id);
        let Some(job) = job else {
            return Ok(
                serde_json::json!({"success": false, "code": "NOT_FOUND", "job_id": job_id}),
            );
        };
        if job
            .get("provider")
            .and_then(|v| v.get("tool"))
            .and_then(|v| v.as_str())
            == Some("mcp_ssh_manager")
        {
            if let Some(ssh) = &self.ssh_manager {
                let wait = ssh.handle_action(args.clone()).await?;
                if let Some(status) = wait.get("status") {
                    if status
                        .get("exited")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        let next = if status.get("exit_code").and_then(|v| v.as_i64()) == Some(0) {
                            "succeeded"
                        } else {
                            "failed"
                        };
                        let _ = self.job_service.upsert(serde_json::json!({
                            "job_id": job_id,
                            "status": next,
                            "ended_at": job.get("ended_at").cloned().unwrap_or(Value::String(now_iso())),
                        }));
                    }
                }
                return Ok(
                    serde_json::json!({"success": true, "job": public_job_view(self.job_service.get(&job_id).as_ref().unwrap_or(&Value::Null)), "wait": wait}),
                );
            }
            return Err(ToolError::internal("SSH manager is not available"));
        }

        let budget_ms = read_positive_int(Some(&Value::String(
            std::env::var("INFRA_TOOL_CALL_TIMEOUT_MS").unwrap_or_else(|_| "55000".to_string()),
        )))
        .unwrap_or(55_000);
        let requested = read_positive_int(args.get("timeout_ms")).unwrap_or(30_000);
        let timeout_ms = std::cmp::min(requested, budget_ms);
        let poll_ms = std::cmp::min(
            read_positive_int(args.get("poll_interval_ms")).unwrap_or(1000),
            5000,
        );
        let started = chrono::Utc::now().timestamp_millis();

        loop {
            let elapsed = (chrono::Utc::now().timestamp_millis() - started) as u64;
            if elapsed + poll_ms > timeout_ms {
                break;
            }
            let current = self.job_service.get(&job_id);
            if let Some(current) = current {
                let status = current.get("status").and_then(|v| v.as_str()).unwrap_or("");
                if status == "succeeded" || status == "failed" || status == "canceled" {
                    return Ok(serde_json::json!({
                        "success": true,
                        "job": public_job_view(&current),
                        "wait": {"completed": true, "timed_out": false, "waited_ms": elapsed, "timeout_ms": timeout_ms, "poll_interval_ms": poll_ms},
                    }));
                }
            } else {
                return Ok(
                    serde_json::json!({"success": false, "code": "NOT_FOUND", "job_id": job_id}),
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
        }

        let current = self.job_service.get(&job_id).unwrap_or(Value::Null);
        Ok(serde_json::json!({
            "success": true,
            "job": public_job_view(&current),
            "wait": {"completed": false, "timed_out": true, "waited_ms": (chrono::Utc::now().timestamp_millis() - started) as u64, "timeout_ms": timeout_ms, "poll_interval_ms": poll_ms},
        }))
    }

    async fn job_logs_tail(&self, args: Value) -> Result<Value, ToolError> {
        let job_id = self.ensure_job_id(args.get("job_id").unwrap_or(&Value::Null))?;
        let job = self.job_service.get(&job_id);
        let Some(job) = job else {
            return Ok(
                serde_json::json!({"success": false, "code": "NOT_FOUND", "job_id": job_id}),
            );
        };
        if job
            .get("provider")
            .and_then(|v| v.get("tool"))
            .and_then(|v| v.as_str())
            == Some("mcp_ssh_manager")
        {
            if let Some(ssh) = &self.ssh_manager {
                let logs = ssh.handle_action(args.clone()).await?;
                return Ok(
                    serde_json::json!({"success": true, "job": public_job_view(&job), "logs": logs}),
                );
            }
            return Err(ToolError::internal("SSH manager is not available"));
        }
        Ok(
            serde_json::json!({"success": false, "code": "NOT_SUPPORTED", "job_id": job_id, "kind": job.get("kind").cloned().unwrap_or(Value::Null)}),
        )
    }

    async fn tail_job(&self, args: Value) -> Result<Value, ToolError> {
        self.job_logs_tail(args).await
    }

    async fn follow_job(&self, args: Value) -> Result<Value, ToolError> {
        let mut next = args.clone();
        if let Value::Object(map) = &mut next {
            map.insert("action".to_string(), Value::String("job_wait".to_string()));
        }
        self.job_wait(next).await
    }

    async fn job_cancel(&self, args: Value) -> Result<Value, ToolError> {
        let job_id = self.ensure_job_id(args.get("job_id").unwrap_or(&Value::Null))?;
        let reason = args.get("reason").and_then(|v| v.as_str());
        let canceled = self.job_service.cancel(&job_id, reason);
        if canceled.is_none() {
            return Ok(
                serde_json::json!({"success": false, "code": "NOT_FOUND", "job_id": job_id}),
            );
        }
        Ok(serde_json::json!({"success": true, "job": public_job_view(canceled.as_ref().unwrap())}))
    }

    async fn job_forget(&self, args: Value) -> Result<Value, ToolError> {
        let job_id = self.ensure_job_id(args.get("job_id").unwrap_or(&Value::Null))?;
        let removed = self.job_service.forget(&job_id);
        Ok(serde_json::json!({"success": removed, "job_id": job_id}))
    }

    async fn job_list(&self, args: Value) -> Result<Value, ToolError> {
        let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(20) as usize;
        let status = args.get("status").and_then(|v| v.as_str());
        let items = self.job_service.list(limit, status);
        Ok(serde_json::json!({"success": true, "jobs": items}))
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for JobManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
