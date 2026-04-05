use crate::errors::ToolError;
use crate::services::capability::CapabilityService;
use crate::services::runbook::RunbookService;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct DescriptionService;

impl DescriptionService {
    pub fn snapshot(
        capability_service: &CapabilityService,
        runbook_service: &RunbookService,
    ) -> Result<Value, ToolError> {
        let capabilities = capability_service.manifest_metadata();
        let runbooks = runbook_service.manifest_metadata();
        let source_payload = serde_json::json!({
            "capabilities": capabilities,
            "runbooks": runbooks,
        });
        let encoded = serde_json::to_vec(&source_payload).map_err(|err| {
            ToolError::internal(format!(
                "Failed to serialize description snapshot source payload: {}",
                err
            ))
        })?;
        let hash = format!("{:x}", Sha256::digest(encoded));

        Ok(serde_json::json!({
            "hash": hash,
            "version": {
                "capabilities": source_payload
                    .get("capabilities")
                    .and_then(|value| value.get("manifest_version"))
                    .cloned()
                    .unwrap_or(Value::Null),
                "runbooks": source_payload
                    .get("runbooks")
                    .and_then(|value| value.get("manifest_version"))
                    .cloned()
                    .unwrap_or(Value::Null),
            },
            "sources": source_payload,
            "loaded_at": chrono::Utc::now().to_rfc3339(),
        }))
    }
}
