use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::utils::paths::resolve_audit_path;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AuditService {
    logger: Logger,
    file_path: std::path::PathBuf,
    queue: Arc<Mutex<()>>,
    stats: Arc<Mutex<AuditStats>>,
}

#[derive(Debug, Default, Clone)]
pub struct AuditStats {
    pub logged: u64,
    pub errors: u64,
    pub reads: u64,
    pub cleared: u64,
}

impl AuditService {
    pub fn new(logger: Logger) -> Self {
        Self {
            logger: logger.child("audit"),
            file_path: resolve_audit_path(),
            queue: Arc::new(Mutex::new(())),
            stats: Arc::new(Mutex::new(AuditStats::default())),
        }
    }

    pub fn append(&self, entry: &Value) {
        let payload = format!("{}\n", entry);
        let _guard = self.queue.lock();
        if let Some(parent) = self.file_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(payload.as_bytes())
            })
        {
            if let Ok(mut stats) = self.stats.lock() {
                stats.errors += 1;
            }
            self.logger.warn(
                "Audit write failed",
                Some(&serde_json::json!({"error": err.to_string()})),
            );
        } else if let Ok(mut stats) = self.stats.lock() {
            stats.logged += 1;
        }
    }

    pub fn clear(&self) -> Result<Value, ToolError> {
        if self.file_path.exists() {
            std::fs::remove_file(&self.file_path).map_err(|err| {
                ToolError::internal(format!("Failed to clear audit file: {}", err))
            })?;
        }
        if let Ok(mut stats) = self.stats.lock() {
            stats.cleared += 1;
        }
        Ok(serde_json::json!({"success": true}))
    }

    pub fn read_entries(
        &self,
        limit: usize,
        offset: usize,
        reverse: bool,
        filters: &Value,
    ) -> Result<Value, ToolError> {
        let mut entries = Vec::new();
        let mut total = 0usize;
        let matcher = build_filter(filters);
        if let Ok(mut stats) = self.stats.lock() {
            stats.reads += 1;
        }
        if self.file_path.exists() {
            let file = std::fs::File::open(&self.file_path).map_err(|err| {
                ToolError::internal(format!("Failed to open audit file: {}", err))
            })?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line.unwrap_or_default();
                if line.trim().is_empty() {
                    continue;
                }
                let parsed: Value = match serde_json::from_str(&line) {
                    Ok(val) => val,
                    Err(_) => {
                        self.logger.warn("Skipping invalid audit entry", None);
                        continue;
                    }
                };
                if !matcher(&parsed) {
                    continue;
                }
                total += 1;
                if reverse {
                    entries.push(parsed);
                    continue;
                }
                if total > offset && entries.len() < limit {
                    entries.push(parsed);
                }
            }
        }
        let final_entries = if reverse {
            let mut reversed = entries;
            reversed.reverse();
            reversed
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect::<Vec<_>>()
        } else {
            entries
        };
        Ok(serde_json::json!({
            "success": true,
            "total": total,
            "offset": offset,
            "limit": limit,
            "entries": final_entries,
        }))
    }

    pub fn stats(&self) -> Value {
        let stats = self.stats.lock().unwrap_or_else(|err| err.into_inner());
        serde_json::json!({
            "logged": stats.logged,
            "errors": stats.errors,
            "reads": stats.reads,
            "cleared": stats.cleared,
            "path": self.file_path,
        })
    }
}

fn build_filter(filters: &Value) -> impl Fn(&Value) -> bool {
    let trace_id = filters
        .get("trace_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let tool = filters
        .get("tool")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let action = filters
        .get("action")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let status = filters
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let since = filters
        .get("since")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let since_ts = since
        .as_ref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp_millis());
    move |entry: &Value| {
        if let Some(trace_id) = trace_id.as_ref() {
            if entry.get("trace_id").and_then(|v| v.as_str()) != Some(trace_id.as_str()) {
                return false;
            }
        }
        if let Some(tool) = tool.as_ref() {
            if entry.get("tool").and_then(|v| v.as_str()) != Some(tool.as_str()) {
                return false;
            }
        }
        if let Some(action) = action.as_ref() {
            if entry.get("action").and_then(|v| v.as_str()) != Some(action.as_str()) {
                return false;
            }
        }
        if let Some(status) = status.as_ref() {
            if entry.get("status").and_then(|v| v.as_str()) != Some(status.as_str()) {
                return false;
            }
        }
        if let Some(since_ts) = since_ts {
            if let Some(ts) = entry.get("timestamp").and_then(|v| v.as_str()) {
                if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) {
                    if parsed.timestamp_millis() < since_ts {
                        return false;
                    }
                }
            }
        }
        true
    }
}
