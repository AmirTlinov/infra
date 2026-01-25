use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::security::Security;
use crate::utils::paths::resolve_evidence_dir;
use rand::{distributions::Alphanumeric, Rng};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn build_evidence_id() -> String {
    let mut rng = rand::thread_rng();
    (0..16).map(|_| rng.sample(Alphanumeric) as char).collect()
}

fn safe_timestamp() -> String {
    chrono::Utc::now().to_rfc3339().replace([':', '.'], "-")
}

#[derive(Clone)]
pub struct EvidenceService {
    logger: Logger,
    security: Security,
    base_dir: PathBuf,
}

impl EvidenceService {
    pub fn new(logger: Logger, security: Security) -> Self {
        Self {
            logger: logger.child("evidence"),
            security,
            base_dir: resolve_evidence_dir(),
        }
    }

    fn ensure_dir(&self) -> Result<(), ToolError> {
        std::fs::create_dir_all(&self.base_dir).map_err(|err| {
            ToolError::internal(format!("Failed to create evidence dir: {}", err))
        })?;
        Ok(())
    }

    pub fn save_evidence(&self, bundle: &Value) -> Result<Value, ToolError> {
        self.logger.debug("save_evidence", None);
        self.ensure_dir()?;
        let payload = serde_json::to_string_pretty(bundle)
            .map_err(|err| ToolError::internal(format!("Failed to serialize evidence: {}", err)))?;
        self.security.ensure_size_fits(&payload, None)?;
        let filename = format!("evidence-{}-{}.json", safe_timestamp(), build_evidence_id());
        let full_path = self.base_dir.join(&filename);
        std::fs::write(&full_path, format!("{}\n", payload))
            .map_err(|err| ToolError::internal(format!("Failed to write evidence: {}", err)))?;
        Ok(serde_json::json!({"id": filename, "path": full_path}))
    }

    pub fn list_evidence(&self) -> Result<Vec<String>, ToolError> {
        let mut entries = Vec::new();
        if !self.base_dir.exists() {
            return Ok(entries);
        }
        for entry in (std::fs::read_dir(&self.base_dir)
            .map_err(|err| ToolError::internal(format!("Failed to read evidence dir: {}", err)))?)
        .flatten()
        {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.ends_with(".json") {
                        entries.push(name.to_string());
                    }
                }
            }
        }
        entries.sort();
        entries.reverse();
        Ok(entries)
    }

    pub fn get_evidence(&self, id: &str) -> Result<Value, ToolError> {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return Err(ToolError::invalid_params(
                "Evidence id must be a non-empty string",
            ));
        }
        let filename = Path::new(trimmed)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if filename.is_empty() {
            return Err(ToolError::invalid_params(
                "Evidence id must be a valid filename",
            ));
        }
        let full_path = self.base_dir.join(filename);
        let raw = std::fs::read_to_string(&full_path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                ToolError::not_found(format!("Evidence not found: {}", filename)).with_hint(
                    "Use action=evidence_list to see recent evidence bundles.".to_string(),
                )
            } else {
                ToolError::internal(format!("Failed to read evidence: {}", err))
            }
        })?;
        self.security.ensure_size_fits(&raw, None)?;
        let payload: Value = serde_json::from_str(&raw)
            .map_err(|err| ToolError::internal(format!("Failed to parse evidence: {}", err)))?;
        Ok(serde_json::json!({"id": filename, "path": full_path, "payload": payload}))
    }
}
