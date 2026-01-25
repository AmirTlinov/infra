use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::utils::fs_atomic::{atomic_write_text_file, temp_sibling_path};
use crate::utils::paths::resolve_cache_dir;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct CacheService {
    logger: Logger,
    cache_dir: PathBuf,
    stats: Arc<Mutex<CacheStats>>,
}

#[derive(Default)]
struct CacheStats {
    hits: u64,
    misses: u64,
    writes: u64,
    errors: u64,
}

impl CacheService {
    pub fn new(logger: Logger) -> Self {
        Self {
            logger: logger.child("cache"),
            cache_dir: resolve_cache_dir(),
            stats: Arc::new(Mutex::new(CacheStats::default())),
        }
    }

    pub fn ensure_key(&self, key: &str) -> Result<String, ToolError> {
        let normalized = key.trim().to_lowercase();
        let valid = normalized.len() == 64 && normalized.chars().all(|c| c.is_ascii_hexdigit());
        if !valid {
            return Err(ToolError::invalid_params(
                "Cache key must be a sha256 hex string",
            )
            .with_hint(
                "Provide a 64-char hex digest, or omit key to auto-generate it from the request.".to_string(),
            ));
        }
        Ok(normalized)
    }

    pub fn normalize_key(&self, key: Option<&Value>) -> Option<String> {
        let key = key?;
        if key.is_null() {
            return None;
        }
        if let Some(text) = key.as_str() {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return None;
            }
            if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
                return Some(trimmed.to_lowercase());
            }
            return Some(self.build_key(&Value::String(trimmed.to_string())));
        }
        Some(self.build_key(key))
    }

    pub fn build_key(&self, input: &Value) -> String {
        let payload = stable_stringify(input);
        let mut hasher = Sha256::new();
        hasher.update(payload.as_bytes());
        hex::encode(hasher.finalize())
    }

    fn entry_path(&self, key: &str) -> Result<PathBuf, ToolError> {
        let normalized = self.ensure_key(key)?;
        Ok(self.cache_dir.join(format!("{}.json", normalized)))
    }

    pub fn data_path(&self, key: &str) -> Result<PathBuf, ToolError> {
        let normalized = self.ensure_key(key)?;
        Ok(self.cache_dir.join(format!("{}.bin", normalized)))
    }

    fn is_expired(meta: &Value, ttl_override: Option<u64>) -> bool {
        let ttl = ttl_override.or_else(|| meta.get("ttl_ms").and_then(|v| v.as_u64()));
        let created_at = meta.get("created_at").and_then(|v| v.as_str());
        if ttl.is_none() || created_at.is_none() {
            return false;
        }
        let created = chrono::DateTime::parse_from_rfc3339(created_at.unwrap()).ok();
        if created.is_none() {
            return false;
        }
        let elapsed = chrono::Utc::now().timestamp_millis() - created.unwrap().timestamp_millis();
        elapsed > ttl.unwrap() as i64
    }

    pub fn get_json(&self, key: &str, ttl_ms: Option<u64>) -> Result<Option<Value>, ToolError> {
        let entry_path = self.entry_path(key)?;
        let raw = match std::fs::read_to_string(&entry_path) {
            Ok(raw) => raw,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    self.bump_errors();
                    self.logger.warn(
                        "Cache read failed",
                        Some(&serde_json::json!({"error": err.to_string()})),
                    );
                }
                self.bump_misses();
                return Ok(None);
            }
        };
        let payload: Value = match serde_json::from_str(&raw) {
            Ok(val) => val,
            Err(_) => {
                self.bump_misses();
                return Ok(None);
            }
        };
        if payload.get("type").and_then(|v| v.as_str()) != Some("json") {
            self.bump_misses();
            return Ok(None);
        }
        if Self::is_expired(&payload, ttl_ms) {
            let _ = self.remove(key);
            self.bump_misses();
            return Ok(None);
        }
        self.bump_hits();
        Ok(Some(payload))
    }

    pub fn get_file(&self, key: &str, ttl_ms: Option<u64>) -> Result<Option<Value>, ToolError> {
        let entry_path = self.entry_path(key)?;
        let raw = match std::fs::read_to_string(&entry_path) {
            Ok(raw) => raw,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    self.bump_errors();
                    self.logger.warn(
                        "Cache read failed",
                        Some(&serde_json::json!({"error": err.to_string()})),
                    );
                }
                self.bump_misses();
                return Ok(None);
            }
        };
        let payload: Value = match serde_json::from_str(&raw) {
            Ok(val) => val,
            Err(_) => {
                self.bump_misses();
                return Ok(None);
            }
        };
        if payload.get("type").and_then(|v| v.as_str()) != Some("file") {
            self.bump_misses();
            return Ok(None);
        }
        if Self::is_expired(&payload, ttl_ms) {
            let _ = self.remove(key);
            self.bump_misses();
            return Ok(None);
        }
        let data_path = self.data_path(key)?;
        let mut entry = payload.clone();
        if let Value::Object(map) = &mut entry {
            map.insert(
                "file_path".to_string(),
                Value::String(data_path.display().to_string()),
            );
        }
        self.bump_hits();
        Ok(Some(entry))
    }

    pub fn set_json(
        &self,
        key: &str,
        value: &Value,
        ttl_ms: Option<u64>,
        meta: Option<Value>,
    ) -> Result<Value, ToolError> {
        let payload = serde_json::json!({
            "type": "json",
            "created_at": chrono::Utc::now().to_rfc3339(),
            "ttl_ms": ttl_ms,
            "meta": meta,
            "value": value,
        });
        let serialized = serde_json::to_string_pretty(&payload).map_err(|err| {
            ToolError::internal(format!("Failed to serialize cache entry: {}", err))
        })?;
        atomic_write_text_file(self.entry_path(key)?, &format!("{}\n", serialized), 0o600)
            .map_err(|err| ToolError::internal(format!("Failed to write cache entry: {}", err)))?;
        self.bump_writes();
        Ok(payload)
    }

    pub fn create_file_writer(
        &self,
        key: &str,
        _ttl_ms: Option<u64>,
        _meta: Option<Value>,
    ) -> Result<(PathBuf, PathBuf), ToolError> {
        let key = self.ensure_key(key)?;
        std::fs::create_dir_all(&self.cache_dir)
            .map_err(|err| ToolError::internal(format!("Failed to create cache dir: {}", err)))?;
        let data_path = self.cache_dir.join(format!("{}.bin", key));
        let tmp_path = temp_sibling_path(&data_path);
        Ok((data_path, tmp_path))
    }

    pub fn finalize_file_writer(
        &self,
        key: &str,
        tmp_path: &Path,
        ttl_ms: Option<u64>,
        meta: Option<Value>,
    ) -> Result<Value, ToolError> {
        let data_path = self.data_path(key)?;
        std::fs::rename(tmp_path, &data_path).map_err(|err| {
            ToolError::internal(format!("Failed to finalize cache file: {}", err))
        })?;
        let payload = serde_json::json!({
            "type": "file",
            "created_at": chrono::Utc::now().to_rfc3339(),
            "ttl_ms": ttl_ms,
            "meta": meta,
        });
        let serialized = serde_json::to_string_pretty(&payload).map_err(|err| {
            ToolError::internal(format!("Failed to serialize cache entry: {}", err))
        })?;
        atomic_write_text_file(self.entry_path(key)?, &format!("{}\n", serialized), 0o600)
            .map_err(|err| ToolError::internal(format!("Failed to write cache entry: {}", err)))?;
        self.bump_writes();
        Ok(payload)
    }

    pub fn remove(&self, key: &str) -> Result<(), ToolError> {
        if let Ok(path) = self.entry_path(key) {
            let _ = std::fs::remove_file(path);
        }
        if let Ok(path) = self.data_path(key) {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }

    fn bump_hits(&self) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.hits += 1;
        }
    }

    fn bump_misses(&self) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.misses += 1;
        }
    }

    fn bump_writes(&self) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.writes += 1;
        }
    }

    fn bump_errors(&self) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.errors += 1;
        }
    }
}

fn stable_stringify(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| s.clone()),
        Value::Array(arr) => {
            let inner: Vec<String> = arr.iter().map(stable_stringify).collect();
            format!("[{}]", inner.join(","))
        }
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_default(),
                        stable_stringify(&map[*key])
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}
