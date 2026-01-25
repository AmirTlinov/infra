use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct MemoryJobStore {
    jobs: Arc<RwLock<HashMap<String, Value>>>,
    max_jobs: usize,
    ttl_ms: u64,
    source: String,
}

impl MemoryJobStore {
    pub fn new(max_jobs: usize, ttl_ms: u64, source: &str) -> Self {
        Self {
            jobs: Arc::new(RwLock::new(HashMap::new())),
            max_jobs,
            ttl_ms,
            source: source.to_string(),
        }
    }

    pub fn create(&self, job: Value) -> Value {
        let job_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let expires_at_ms = chrono::Utc::now().timestamp_millis() + self.ttl_ms as i64;
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
        self.insert(job_id.clone(), record.clone());
        record
    }

    pub fn upsert(&self, job: Value) -> Option<Value> {
        let job_id = job
            .get("job_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())?;
        let now = chrono::Utc::now().to_rfc3339();
        let expires_at_ms = chrono::Utc::now().timestamp_millis() + self.ttl_ms as i64;
        let mut record = job;
        if let Value::Object(map) = &mut record {
            map.insert("job_id".to_string(), Value::String(job_id.clone()));
            map.insert("updated_at".to_string(), Value::String(now));
            map.insert(
                "expires_at_ms".to_string(),
                Value::Number(serde_json::Number::from(expires_at_ms)),
            );
            map.entry("created_at".to_string())
                .or_insert(Value::String(chrono::Utc::now().to_rfc3339()));
            map.entry("status".to_string())
                .or_insert(Value::String("queued".to_string()));
        }
        self.insert(job_id, record.clone());
        Some(record)
    }

    pub fn get(&self, job_id: &str) -> Option<Value> {
        self.purge_expired();
        self.jobs.read().unwrap().get(job_id).cloned()
    }

    pub fn has(&self, job_id: &str) -> bool {
        self.jobs.read().unwrap().contains_key(job_id)
    }

    pub fn list(&self, limit: usize, status: Option<&str>) -> Vec<Value> {
        self.purge_expired();
        let mut values: Vec<Value> = self.jobs.read().unwrap().values().cloned().collect();
        values.reverse();
        let mut out = Vec::new();
        for job in values {
            if let Some(status_filter) = status {
                if job.get("status").and_then(|v| v.as_str()) != Some(status_filter) {
                    continue;
                }
            }
            out.push(job);
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    pub fn forget(&self, job_id: &str) -> bool {
        self.jobs.write().unwrap().remove(job_id).is_some()
    }

    pub fn load(&self, records: &[Value]) {
        for record in records {
            if let Some(job_id) = record.get("job_id").and_then(|v| v.as_str()) {
                self.insert(job_id.to_string(), record.clone());
            }
        }
    }

    pub fn to_json(&self) -> Value {
        let jobs: Vec<Value> = self.jobs.read().unwrap().values().cloned().collect();
        serde_json::json!({
            "version": 1,
            "updated_at": chrono::Utc::now().to_rfc3339(),
            "jobs": jobs,
        })
    }

    pub fn stats(&self) -> Value {
        serde_json::json!({
            "jobs": self.jobs.read().unwrap().len(),
            "max_jobs": self.max_jobs,
            "ttl_ms": self.ttl_ms,
            "store": self.source,
        })
    }

    pub fn purge_expired(&self) {
        let now = chrono::Utc::now().timestamp_millis();
        self.jobs.write().unwrap().retain(|_, job| {
            job.get("expires_at_ms")
                .and_then(|v| v.as_i64())
                .map(|ts| ts > now)
                .unwrap_or(true)
        });
        while self.jobs.read().unwrap().len() > self.max_jobs {
            if let Some(key) = self.jobs.write().unwrap().keys().next().cloned() {
                self.jobs.write().unwrap().remove(&key);
            } else {
                break;
            }
        }
    }

    fn insert(&self, job_id: String, job: Value) {
        self.jobs.write().unwrap().insert(job_id, job);
        self.purge_expired();
    }
}
