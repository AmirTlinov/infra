use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::utils::artifacts::{resolve_artifact_path, resolve_context_root};
use crate::utils::feature_flags::is_allow_secret_export_enabled;
use crate::utils::redact::redact_text;
use crate::utils::tool_errors::unknown_action_error;
use base64::Engine;
use serde_json::Value;
use std::io::Seek;
use std::path::PathBuf;

const ARTIFACT_ACTIONS: &[&str] = &["get", "head", "tail", "list"];

fn allow_secret_export() -> bool {
    is_allow_secret_export_enabled()
}

fn ensure_secret_export_allowed(include: bool) -> Result<(), ToolError> {
    if !include {
        return Ok(());
    }
    if allow_secret_export() {
        return Ok(());
    }
    Err(
        ToolError::denied("include_secrets=true is disabled for artifacts").with_hint(
            "Set INFRA_ALLOW_SECRET_EXPORT=1 to enable break-glass secret export.".to_string(),
        ),
    )
}

fn read_positive_int(value: Option<&Value>) -> Option<usize> {
    let value = value?;
    if let Some(n) = value.as_i64() {
        if n > 0 {
            return Some(n as usize);
        }
    }
    if let Some(text) = value.as_str() {
        if let Ok(parsed) = text.parse::<usize>() {
            if parsed > 0 {
                return Some(parsed);
            }
        }
    }
    None
}

fn normalize_artifact_rel(uri: Option<&Value>, rel: Option<&Value>) -> Result<String, ToolError> {
    if let Some(Value::String(text)) = uri {
        let trimmed = text.trim();
        if !trimmed.starts_with("artifact://") {
            return Err(ToolError::invalid_params("uri must start with artifact://"));
        }
        let next = trimmed.trim_start_matches("artifact://");
        if next.trim().is_empty() {
            return Err(ToolError::invalid_params("artifact uri must include path"));
        }
        return Ok(next.trim().to_string());
    }
    if let Some(Value::String(text)) = rel {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    Err(ToolError::invalid_params("Provide artifact uri or rel path").with_hint(
        "Example: { action: 'get', uri: 'artifact://runs/<trace>/tool_calls/<span>/result.json' }".to_string(),
    ))
}

fn build_artifact_uri(rel: &str) -> String {
    format!("artifact://{}", rel)
}

fn resolve_file_path(
    uri: Option<&Value>,
    rel: Option<&Value>,
) -> Result<(PathBuf, String, String), ToolError> {
    let context_root = resolve_context_root().ok_or_else(|| {
        ToolError::denied("Artifacts are unavailable (context repo root is not configured)")
            .with_hint(
                "Set INFRA_CONTEXT_REPO_ROOT (or MCP_CONTEXT_REPO_ROOT) to a writable directory."
                    .to_string(),
            )
    })?;
    let artifact_rel = normalize_artifact_rel(uri, rel)?;
    let file_path = resolve_artifact_path(&context_root, &artifact_rel)?;
    if !file_path.exists() {
        return Err(ToolError::not_found(format!(
            "Artifact not found: {}",
            build_artifact_uri(&artifact_rel)
        ))
        .with_hint(
            "Check the uri/rel or call { action: 'list' } to discover available artifacts."
                .to_string(),
        ));
    }
    Ok((
        file_path,
        artifact_rel.clone(),
        build_artifact_uri(&artifact_rel),
    ))
}

fn read_file_slice(
    path: &PathBuf,
    offset: usize,
    length: usize,
) -> Result<(Vec<u8>, u64, usize, usize, bool), ToolError> {
    let mut file = std::fs::File::open(path)
        .map_err(|err| ToolError::invalid_params(format!("file unreadable: {}", err)))?;
    let metadata = file
        .metadata()
        .map_err(|err| ToolError::internal(err.to_string()))?;
    let file_bytes = metadata.len();
    let start = offset.min(file_bytes as usize);
    let to_read = length.min(file_bytes as usize - start);
    let mut buffer = vec![0u8; to_read];
    use std::io::Read;
    if to_read > 0 {
        file.seek(std::io::SeekFrom::Start(start as u64))
            .map_err(|err| ToolError::internal(err.to_string()))?;
        file.read_exact(&mut buffer)
            .map_err(|err| ToolError::internal(err.to_string()))?;
    }
    let truncated = (start + to_read) < file_bytes as usize;
    Ok((buffer, file_bytes, start, to_read, truncated))
}

fn read_file_tail(
    path: &PathBuf,
    length: usize,
) -> Result<(Vec<u8>, u64, usize, usize, bool), ToolError> {
    let metadata = std::fs::metadata(path).map_err(|err| ToolError::internal(err.to_string()))?;
    let file_bytes = metadata.len();
    let to_read = length.min(file_bytes as usize);
    let start = file_bytes as usize - to_read;
    let mut buffer = vec![0u8; to_read];
    let mut file = std::fs::File::open(path)
        .map_err(|err| ToolError::invalid_params(format!("file unreadable: {}", err)))?;
    use std::io::Read;
    if to_read > 0 {
        file.seek(std::io::SeekFrom::Start(start as u64))
            .map_err(|err| ToolError::internal(err.to_string()))?;
        file.read_exact(&mut buffer)
            .map_err(|err| ToolError::internal(err.to_string()))?;
    }
    let truncated = start > 0;
    Ok((buffer, file_bytes, start, to_read, truncated))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[derive(Clone)]
pub struct ArtifactManager {
    logger: Logger,
}

impl ArtifactManager {
    pub fn new(logger: Logger) -> Self {
        Self {
            logger: logger.child("artifacts"),
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "get" => {
                let this = self.clone();
                tokio::task::spawn_blocking(move || this.get(args))
                    .await
                    .map_err(|_| ToolError::internal("Artifacts task failed"))?
            }
            "head" => {
                let this = self.clone();
                tokio::task::spawn_blocking(move || this.head(args))
                    .await
                    .map_err(|_| ToolError::internal("Artifacts task failed"))?
            }
            "tail" => {
                let this = self.clone();
                tokio::task::spawn_blocking(move || this.tail(args))
                    .await
                    .map_err(|_| ToolError::internal("Artifacts task failed"))?
            }
            "list" => {
                let this = self.clone();
                tokio::task::spawn_blocking(move || this.list(args))
                    .await
                    .map_err(|_| ToolError::internal("Artifacts task failed"))?
            }
            _ => Err(unknown_action_error("artifacts", action, ARTIFACT_ACTIONS)),
        }
    }

    fn get(&self, args: Value) -> Result<Value, ToolError> {
        let include_secrets = args
            .get("include_secrets")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        ensure_secret_export_allowed(include_secrets)?;
        let (path, rel, uri) = resolve_file_path(args.get("uri"), args.get("rel"))?;
        let max_bytes = read_positive_int(args.get("max_bytes")).unwrap_or(256 * 1024);
        let offset = read_positive_int(args.get("offset")).unwrap_or(0);
        let (buffer, file_bytes, start, length, truncated) =
            read_file_slice(&path, offset, max_bytes)?;
        let encoding = args
            .get("encoding")
            .and_then(|v| v.as_str())
            .unwrap_or("utf8");
        let mut content = if encoding == "base64" {
            base64::engine::general_purpose::STANDARD.encode(&buffer)
        } else {
            String::from_utf8_lossy(&buffer).to_string()
        };
        if !include_secrets {
            content = redact_text(&content, usize::MAX, None);
        }
        Ok(serde_json::json!({
            "success": true,
            "uri": uri,
            "rel": rel,
            "content": content,
            "encoding": encoding,
            "file_bytes": file_bytes,
            "offset": start,
            "length": length,
            "truncated": truncated,
            "sha256": sha256_hex(&buffer),
        }))
    }

    fn head(&self, args: Value) -> Result<Value, ToolError> {
        let include_secrets = args
            .get("include_secrets")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        ensure_secret_export_allowed(include_secrets)?;
        let (path, rel, uri) = resolve_file_path(args.get("uri"), args.get("rel"))?;
        let max_bytes = read_positive_int(args.get("max_bytes")).unwrap_or(64 * 1024);
        let (buffer, file_bytes, start, length, truncated) = read_file_slice(&path, 0, max_bytes)?;
        let encoding = args
            .get("encoding")
            .and_then(|v| v.as_str())
            .unwrap_or("utf8");
        let mut content = if encoding == "base64" {
            base64::engine::general_purpose::STANDARD.encode(&buffer)
        } else {
            String::from_utf8_lossy(&buffer).to_string()
        };
        if !include_secrets {
            content = redact_text(&content, usize::MAX, None);
        }
        Ok(serde_json::json!({
            "success": true,
            "uri": uri,
            "rel": rel,
            "content": content,
            "encoding": encoding,
            "file_bytes": file_bytes,
            "offset": start,
            "length": length,
            "truncated": truncated,
            "sha256": sha256_hex(&buffer),
        }))
    }

    fn tail(&self, args: Value) -> Result<Value, ToolError> {
        let include_secrets = args
            .get("include_secrets")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        ensure_secret_export_allowed(include_secrets)?;
        let (path, rel, uri) = resolve_file_path(args.get("uri"), args.get("rel"))?;
        let max_bytes = read_positive_int(args.get("max_bytes")).unwrap_or(64 * 1024);
        let (buffer, file_bytes, start, length, truncated) = read_file_tail(&path, max_bytes)?;
        let encoding = args
            .get("encoding")
            .and_then(|v| v.as_str())
            .unwrap_or("utf8");
        let mut content = if encoding == "base64" {
            base64::engine::general_purpose::STANDARD.encode(&buffer)
        } else {
            String::from_utf8_lossy(&buffer).to_string()
        };
        if !include_secrets {
            content = redact_text(&content, usize::MAX, None);
        }
        Ok(serde_json::json!({
            "success": true,
            "uri": uri,
            "rel": rel,
            "content": content,
            "encoding": encoding,
            "file_bytes": file_bytes,
            "offset": start,
            "length": length,
            "truncated": truncated,
            "sha256": sha256_hex(&buffer),
        }))
    }

    fn list(&self, args: Value) -> Result<Value, ToolError> {
        let context_root = resolve_context_root().ok_or_else(|| {
            ToolError::denied("Artifacts are unavailable (context repo root is not configured)")
                .with_hint(
                "Set INFRA_CONTEXT_REPO_ROOT (or MCP_CONTEXT_REPO_ROOT) to a writable directory."
                    .to_string(),
            )
        })?;
        let prefix = args
            .get("prefix")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let limit = read_positive_int(args.get("limit")).unwrap_or(100);
        let artifacts_root = context_root.join("artifacts");
        let search_root = if prefix.is_empty() {
            artifacts_root.clone()
        } else {
            artifacts_root.join(&prefix)
        };
        if !search_root.exists() {
            return Ok(
                serde_json::json!({"success": true, "entries": [], "prefix": prefix, "total": 0}),
            );
        }
        let mut entries = Vec::new();
        for entry in walkdir::WalkDir::new(&search_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let rel_path = entry
                .path()
                .strip_prefix(&artifacts_root)
                .unwrap_or(entry.path());
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            entries.push(serde_json::json!({
                "uri": build_artifact_uri(&rel),
                "rel": rel,
            }));
            if entries.len() >= limit {
                break;
            }
        }
        Ok(serde_json::json!({
            "success": true,
            "entries": entries,
            "prefix": prefix,
            "total": entries.len(),
        }))
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for ArtifactManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
