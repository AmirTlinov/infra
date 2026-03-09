use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::store_db::{StoreDb, StoreRecord};
use crate::utils::paths::resolve_jobs_path;
use serde_json::Value;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};

const NAMESPACE: &str = "jobs";

#[derive(Clone)]
pub struct JobService {
    logger: Logger,
    store: StoreDb,
    max_jobs: usize,
    ttl_ms: u64,
    abort_flags: Arc<RwLock<HashMap<String, Arc<AtomicBool>>>>,
}

impl JobService {
    pub fn new(logger: Logger) -> Result<Self, ToolError> {
        let service = Self {
            logger: logger.child("jobs"),
            store: StoreDb::new()?,
            max_jobs: std::env::var("INFRA_JOBS_MAX")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(500),
            ttl_ms: std::env::var("INFRA_JOBS_TTL_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(6 * 60 * 60_000),
            abort_flags: Arc::new(RwLock::new(HashMap::new())),
        };
        service.import_legacy_once()?;
        service.purge_expired()?;
        service.enforce_max_jobs()?;
        Ok(service)
    }

    fn import_legacy_once(&self) -> Result<(), ToolError> {
        let path = resolve_jobs_path();
        let import_key = format!("file:{}", path.display());
        if self.store.has_import(NAMESPACE, &import_key)? || !path.exists() {
            return Ok(());
        }

        let raw = std::fs::read_to_string(&path)
            .map_err(|err| ToolError::internal(format!("Failed to load jobs store: {}", err)))?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|err| ToolError::internal(format!("Failed to parse jobs store: {}", err)))?;
        let entries = parsed
            .get("jobs")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_else(|| parsed.as_array().cloned().unwrap_or_default());

        for record in entries {
            let Some(job_id) = record
                .get("job_id")
                .and_then(|v| v.as_str())
                .filter(|v| !v.trim().is_empty())
            else {
                continue;
            };
            self.store
                .upsert(NAMESPACE, job_id, &record, Some("legacy_file"))?;
        }
        self.store.mark_imported(NAMESPACE, &import_key)?;
        Ok(())
    }

    fn ensure_abort_flag(&self, job_id: &str) {
        self.abort_flags
            .write()
            .unwrap()
            .entry(job_id.to_string())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)));
    }

    fn now_iso() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn next_expires_at_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis() + self.ttl_ms as i64
    }

    fn job_order_key(job: &Value) -> i64 {
        for field in ["updated_at", "ended_at", "started_at", "created_at"] {
            if let Some(value) = job.get(field).and_then(|v| v.as_str()) {
                if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(value) {
                    return parsed.timestamp_millis();
                }
            }
        }
        0
    }

    fn persist_record(&self, job_id: &str, record: &Value) -> Result<(), ToolError> {
        self.store
            .upsert(NAMESPACE, job_id, record, Some("local"))?;
        self.purge_expired()?;
        self.enforce_max_jobs()?;
        Ok(())
    }

    fn purge_expired(&self) -> Result<(), ToolError> {
        let now = chrono::Utc::now().timestamp_millis();
        for record in self.store.list(NAMESPACE)? {
            let expired = record
                .value
                .get("expires_at_ms")
                .and_then(|v| v.as_i64())
                .map(|ts| ts <= now)
                .unwrap_or(false);
            if expired {
                let _ = self.store.delete(NAMESPACE, &record.key)?;
                self.abort_flags.write().unwrap().remove(&record.key);
            }
        }
        Ok(())
    }

    fn enforce_max_jobs(&self) -> Result<(), ToolError> {
        let mut records = self.store.list(NAMESPACE)?;
        if records.len() <= self.max_jobs {
            return Ok(());
        }
        records.sort_by_key(|record| Reverse(Self::job_order_key(&record.value)));
        for record in records.into_iter().skip(self.max_jobs) {
            let _ = self.store.delete(NAMESPACE, &record.key)?;
            self.abort_flags.write().unwrap().remove(&record.key);
        }
        Ok(())
    }

    fn normalize_upsert_record(&self, job: Value) -> Option<Value> {
        let job_id = job
            .get("job_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())?;
        let now = Self::now_iso();
        let expires_at_ms = self.next_expires_at_ms();
        let mut record = job;
        if let Value::Object(map) = &mut record {
            map.insert("job_id".to_string(), Value::String(job_id.clone()));
            map.insert("updated_at".to_string(), Value::String(now.clone()));
            map.insert(
                "expires_at_ms".to_string(),
                Value::Number(serde_json::Number::from(expires_at_ms)),
            );
            map.entry("created_at".to_string())
                .or_insert(Value::String(now.clone()));
            map.entry("status".to_string())
                .or_insert(Value::String("queued".to_string()));
        }
        Some(record)
    }

    fn log_store_error(&self, operation: &str, err: &ToolError) {
        self.logger.error(
            operation,
            Some(&serde_json::json!({
                "message": err.message,
                "code": err.code,
            })),
        );
    }

    fn list_records(&self) -> Result<Vec<StoreRecord>, ToolError> {
        self.purge_expired()?;
        self.store.list(NAMESPACE)
    }

    pub fn create(&self, job: Value) -> Value {
        self.logger.debug("create", None);
        let job_id = uuid::Uuid::new_v4().to_string();
        let now = Self::now_iso();
        let expires_at_ms = self.next_expires_at_ms();
        let record = serde_json::json!({
            "job_id": job_id,
            "kind": job.get("kind").cloned().unwrap_or(Value::String("inprocess_task".to_string())),
            "status": "queued",
            "trace_id": job.get("trace_id").cloned().unwrap_or(Value::Null),
            "parent_span_id": job.get("parent_span_id").cloned().unwrap_or(Value::Null),
            "created_at": now,
            "started_at": Value::Null,
            "updated_at": chrono::Utc::now().to_rfc3339(),
            "ended_at": Value::Null,
            "progress": job.get("progress").cloned().unwrap_or(Value::Null),
            "artifacts": Value::Null,
            "provider": job.get("provider").cloned().unwrap_or(Value::Null),
            "error": Value::Null,
            "expires_at_ms": expires_at_ms,
        });

        if let Some(job_id) = record.get("job_id").and_then(|v| v.as_str()) {
            self.ensure_abort_flag(job_id);
            if let Err(err) = self.persist_record(job_id, &record) {
                self.log_store_error("create", &err);
            }
        }
        record
    }

    pub fn upsert(&self, job: Value) -> Option<Value> {
        let record = self.normalize_upsert_record(job)?;
        if let Some(job_id) = record.get("job_id").and_then(|v| v.as_str()) {
            self.ensure_abort_flag(job_id);
            if let Err(err) = self.persist_record(job_id, &record) {
                self.log_store_error("upsert", &err);
            }
        }
        Some(record)
    }

    pub fn get(&self, job_id: &str) -> Option<Value> {
        if let Err(err) = self.purge_expired() {
            self.log_store_error("get.purge_expired", &err);
        }
        match self.store.get(NAMESPACE, job_id) {
            Ok(Some(record)) => Some(record.value),
            Ok(None) => None,
            Err(err) => {
                self.log_store_error("get", &err);
                None
            }
        }
    }

    pub fn list(&self, limit: usize, status: Option<&str>) -> Vec<Value> {
        let records = match self.list_records() {
            Ok(records) => records,
            Err(err) => {
                self.log_store_error("list", &err);
                return Vec::new();
            }
        };
        let mut jobs = records
            .into_iter()
            .map(|record| record.value)
            .filter(|job| {
                status
                    .map(|status_filter| {
                        job.get("status").and_then(|v| v.as_str()) == Some(status_filter)
                    })
                    .unwrap_or(true)
            })
            .collect::<Vec<_>>();
        jobs.sort_by_key(|job| Reverse(Self::job_order_key(job)));
        jobs.truncate(limit);
        jobs
    }

    pub fn forget(&self, job_id: &str) -> bool {
        let existed = match self.store.delete(NAMESPACE, job_id) {
            Ok(existed) => existed,
            Err(err) => {
                self.log_store_error("forget", &err);
                false
            }
        };
        if existed {
            self.abort_flags.write().unwrap().remove(job_id);
        }
        existed
    }

    pub fn get_abort_flag(&self, job_id: &str) -> Arc<AtomicBool> {
        self.abort_flags
            .write()
            .unwrap()
            .entry(job_id.to_string())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    }

    pub fn cancel(&self, job_id: &str, reason: Option<&str>) -> Option<Value> {
        let job = self.get(job_id)?;
        let flag = self.get_abort_flag(job_id);
        flag.store(true, Ordering::SeqCst);
        let mut updated = job.clone();
        if let Value::Object(map) = &mut updated {
            map.insert("status".to_string(), Value::String("canceled".to_string()));
            map.insert(
                "ended_at".to_string(),
                map.get("ended_at")
                    .cloned()
                    .unwrap_or(Value::String(Self::now_iso())),
            );
            if let Some(reason) = reason {
                map.insert("error".to_string(), Value::String(reason.to_string()));
            }
        }
        self.upsert(updated)
    }

    pub fn stats(&self) -> Value {
        let jobs = self.list(self.max_jobs, None).len();
        serde_json::json!({
            "jobs": jobs,
            "max_jobs": self.max_jobs,
            "ttl_ms": self.ttl_ms,
            "store": "sqlite",
            "path": self.store.path(),
        })
    }

    pub fn has_live_jobs(&self) -> bool {
        self.list(self.max_jobs, None).iter().any(|job| {
            matches!(
                job.get("status").and_then(|value| value.as_str()),
                Some("queued") | Some("running")
            )
        })
    }
}
