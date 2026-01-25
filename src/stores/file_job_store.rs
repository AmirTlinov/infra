use crate::errors::ToolError;
use crate::stores::memory_job_store::MemoryJobStore;
use crate::utils::fs_atomic::atomic_write_text_file;
use crate::utils::paths::resolve_jobs_path;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct FileJobStore {
    inner: MemoryJobStore,
    file_path: PathBuf,
    queue: Arc<Mutex<()>>,
}

impl FileJobStore {
    pub fn new(inner: MemoryJobStore) -> Self {
        Self {
            inner,
            file_path: resolve_jobs_path(),
            queue: Arc::new(Mutex::new(())),
        }
    }

    pub fn load_from_disk(&self) -> Result<(), ToolError> {
        if !self.file_path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&self.file_path)
            .map_err(|err| ToolError::internal(format!("Failed to load jobs store: {}", err)))?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|err| ToolError::internal(format!("Failed to parse jobs store: {}", err)))?;
        let entries = parsed
            .get("jobs")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_else(|| parsed.as_array().cloned().unwrap_or_default());
        self.inner.load(&entries);
        Ok(())
    }

    pub fn persist(&self) -> Result<(), ToolError> {
        let payload = serde_json::to_string_pretty(&self.inner.to_json()).map_err(|err| {
            ToolError::internal(format!("Failed to serialize jobs store: {}", err))
        })?;
        let _guard = self.queue.lock();
        atomic_write_text_file(&self.file_path, &format!("{}\n", payload), 0o600)
            .map_err(|err| ToolError::internal(format!("Failed to persist jobs store: {}", err)))?;
        Ok(())
    }

    pub fn inner(&self) -> &MemoryJobStore {
        &self.inner
    }
}
