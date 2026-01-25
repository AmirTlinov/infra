use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::stores::file_job_store::FileJobStore;
use crate::stores::memory_job_store::MemoryJobStore;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};

fn resolve_store_kind() -> String {
    std::env::var("INFRA_JOBS_STORE")
        .unwrap_or_else(|_| "memory".to_string())
        .trim()
        .to_lowercase()
}

#[derive(Clone)]
pub struct JobService {
    logger: Logger,
    memory_store: MemoryJobStore,
    file_store: Option<FileJobStore>,
    abort_flags: Arc<RwLock<HashMap<String, Arc<AtomicBool>>>>,
}

impl JobService {
    pub fn new(logger: Logger) -> Result<Self, ToolError> {
        let kind = resolve_store_kind();
        let max_jobs = std::env::var("INFRA_JOBS_MAX")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(500);
        let ttl_ms = std::env::var("INFRA_JOBS_TTL_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(6 * 60 * 60_000);
        let memory_store = MemoryJobStore::new(
            max_jobs,
            ttl_ms,
            if kind == "file" { "file" } else { "memory" },
        );
        let file_store = if kind == "file" {
            let store = FileJobStore::new(memory_store.clone());
            let _ = store.load_from_disk();
            Some(store)
        } else {
            None
        };
        Ok(Self {
            logger: logger.child("jobs"),
            memory_store,
            file_store,
            abort_flags: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    fn persist(&self) {
        if let Some(store) = &self.file_store {
            let _ = store.persist();
        }
    }

    pub fn create(&self, job: Value) -> Value {
        self.logger.debug("create", None);
        let record = self.memory_store.create(job);
        if let Some(job_id) = record.get("job_id").and_then(|v| v.as_str()) {
            self.abort_flags
                .write()
                .unwrap()
                .entry(job_id.to_string())
                .or_insert_with(|| Arc::new(AtomicBool::new(false)));
        }
        self.persist();
        record
    }

    pub fn upsert(&self, job: Value) -> Option<Value> {
        let record = self.memory_store.upsert(job);
        if let Some(rec) = record.as_ref() {
            if let Some(job_id) = rec.get("job_id").and_then(|v| v.as_str()) {
                self.abort_flags
                    .write()
                    .unwrap()
                    .entry(job_id.to_string())
                    .or_insert_with(|| Arc::new(AtomicBool::new(false)));
            }
        }
        self.persist();
        record
    }

    pub fn get(&self, job_id: &str) -> Option<Value> {
        self.memory_store.get(job_id)
    }

    pub fn list(&self, limit: usize, status: Option<&str>) -> Vec<Value> {
        self.memory_store.list(limit, status)
    }

    pub fn forget(&self, job_id: &str) -> bool {
        let existed = self.memory_store.forget(job_id);
        if existed {
            self.abort_flags.write().unwrap().remove(job_id);
            self.persist();
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
                    .unwrap_or(Value::String(chrono::Utc::now().to_rfc3339())),
            );
            if let Some(reason) = reason {
                map.insert("error".to_string(), Value::String(reason.to_string()));
            }
        }
        let result = self.upsert(updated);
        self.persist();
        result
    }

    pub fn stats(&self) -> Value {
        self.memory_store.stats()
    }
}
