use crate::errors::ToolError;
use crate::utils::fs_atomic::{
    atomic_write_binary_file, atomic_write_text_file, ensure_dir_for_file, temp_sibling_path,
};
use crate::utils::paths::resolve_context_repo_root;
use rand::{distributions::Alphanumeric, Rng};
use std::path::{Path, PathBuf};
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DEFAULT_CONTEXT_REPO_ROOT: &str = "/home/amir/Документы/projects/context";
const DEFAULT_FILE_MODE: u32 = 0o600;

#[derive(Debug, Clone)]
pub struct ArtifactRef {
    pub uri: String,
    pub rel: String,
}

#[derive(Debug, Clone)]
pub struct ArtifactInfo {
    pub uri: String,
    pub rel: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub truncated: bool,
}

#[derive(Debug)]
pub struct ArtifactWriter {
    pub uri: String,
    pub rel: String,
    pub path: PathBuf,
    pub tmp_path: PathBuf,
    pub file: File,
    pub bytes: u64,
}

fn is_directory(candidate: &Path) -> bool {
    candidate.is_dir()
}

pub fn resolve_context_root() -> Option<PathBuf> {
    if let Some(explicit) = resolve_context_repo_root() {
        if is_directory(&explicit) {
            return Some(explicit);
        }
        return None;
    }
    let default_path = PathBuf::from(DEFAULT_CONTEXT_REPO_ROOT);
    if is_directory(&default_path) {
        Some(default_path)
    } else {
        None
    }
}

fn normalize_segment(value: &str, label: &str) -> Result<String, ToolError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ToolError::invalid_params(format!(
            "{} must be a non-empty string",
            label
        )));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(ToolError::invalid_params(format!(
            "{} must not be '.' or '..'",
            label
        )));
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(ToolError::invalid_params(format!(
            "{} must not contain path separators",
            label
        )));
    }
    Ok(trimmed.to_string())
}

fn normalize_filename(value: &str) -> Result<String, ToolError> {
    let trimmed = normalize_segment(value, "filename")?;
    let base = Path::new(&trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if base != trimmed {
        return Err(ToolError::invalid_params(
            "filename must be a basename only",
        ));
    }
    Ok(trimmed)
}

pub fn build_tool_call_context_ref(
    trace_id: Option<&str>,
    span_id: Option<&str>,
) -> Result<ArtifactRef, ToolError> {
    let run_id = normalize_segment(trace_id.unwrap_or("run"), "trace_id")?;
    let call_id = normalize_segment(span_id.unwrap_or(&random_id()), "span_id")?;
    let rel = format!("runs/{}/tool_calls/{}.context", run_id, call_id);
    Ok(ArtifactRef {
        uri: format!("artifact://{}", rel),
        rel,
    })
}

pub fn build_tool_call_file_ref(
    trace_id: Option<&str>,
    span_id: Option<&str>,
    filename: &str,
) -> Result<ArtifactRef, ToolError> {
    let run_id = normalize_segment(trace_id.unwrap_or("run"), "trace_id")?;
    let call_id = normalize_segment(span_id.unwrap_or(&random_id()), "span_id")?;
    let safe_name = normalize_filename(filename)?;
    let rel = format!("runs/{}/tool_calls/{}/{}", run_id, call_id, safe_name);
    Ok(ArtifactRef {
        uri: format!("artifact://{}", rel),
        rel,
    })
}

pub fn resolve_artifact_path(context_root: &Path, rel: &str) -> Result<PathBuf, ToolError> {
    if rel.trim().is_empty() {
        return Err(ToolError::invalid_params(
            "artifact rel must be a non-empty string",
        ));
    }
    let base = context_root.join("artifacts");
    let resolved = base.join(rel);
    if resolved != base && !resolved.starts_with(&base) {
        return Err(ToolError::denied("Artifact path escapes context root")
            .with_hint("Use a rel path within the artifacts root."));
    }
    Ok(resolved)
}

pub fn write_text_artifact(
    context_root: &Path,
    reference: &ArtifactRef,
    content: &str,
) -> Result<ArtifactInfo, ToolError> {
    let path = resolve_artifact_path(context_root, &reference.rel)?;
    atomic_write_text_file(&path, content, DEFAULT_FILE_MODE)
        .map_err(|err| ToolError::internal(format!("Failed to write artifact: {}", err)))?;
    Ok(ArtifactInfo {
        uri: reference.uri.clone(),
        rel: reference.rel.clone(),
        path,
        bytes: content.len() as u64,
        truncated: false,
    })
}

pub fn write_binary_artifact(
    context_root: &Path,
    reference: &ArtifactRef,
    content: &[u8],
) -> Result<ArtifactInfo, ToolError> {
    let path = resolve_artifact_path(context_root, &reference.rel)?;
    atomic_write_binary_file(&path, content, DEFAULT_FILE_MODE)
        .map_err(|err| ToolError::internal(format!("Failed to write artifact: {}", err)))?;
    Ok(ArtifactInfo {
        uri: reference.uri.clone(),
        rel: reference.rel.clone(),
        path,
        bytes: content.len() as u64,
        truncated: false,
    })
}

pub async fn create_artifact_write_stream(
    context_root: &Path,
    reference: &ArtifactRef,
) -> Result<ArtifactWriter, ToolError> {
    let path = resolve_artifact_path(context_root, &reference.rel)?;
    ensure_dir_for_file(&path)
        .map_err(|err| ToolError::internal(format!("Failed to prepare artifact dir: {}", err)))?;
    let tmp_path = temp_sibling_path(&path);
    let file = File::create(&tmp_path)
        .await
        .map_err(|err| ToolError::internal(format!("Failed to create artifact: {}", err)))?;
    Ok(ArtifactWriter {
        uri: reference.uri.clone(),
        rel: reference.rel.clone(),
        path,
        tmp_path,
        file,
        bytes: 0,
    })
}

impl ArtifactWriter {
    pub async fn write(&mut self, chunk: &[u8]) -> Result<(), ToolError> {
        self.file.write_all(chunk).await.map_err(|err| {
            ToolError::internal(format!("Failed to write artifact chunk: {}", err))
        })?;
        self.bytes += chunk.len() as u64;
        Ok(())
    }

    pub async fn finalize(mut self) -> Result<ArtifactInfo, ToolError> {
        self.file
            .flush()
            .await
            .map_err(|err| ToolError::internal(format!("Failed to flush artifact: {}", err)))?;
        drop(self.file);
        fs::rename(&self.tmp_path, &self.path)
            .await
            .map_err(|err| ToolError::internal(format!("Failed to finalize artifact: {}", err)))?;
        Ok(ArtifactInfo {
            uri: self.uri,
            rel: self.rel,
            path: self.path,
            bytes: self.bytes,
            truncated: false,
        })
    }

    pub async fn abort(mut self) -> Result<(), ToolError> {
        let _ = self.file.shutdown().await;
        let _ = fs::remove_file(&self.tmp_path).await;
        Ok(())
    }
}

pub async fn copy_file_artifact(
    context_root: &Path,
    reference: &ArtifactRef,
    source_path: &Path,
    max_bytes: Option<u64>,
) -> Result<ArtifactInfo, ToolError> {
    let path = resolve_artifact_path(context_root, &reference.rel)?;
    ensure_dir_for_file(&path)
        .map_err(|err| ToolError::internal(format!("Failed to prepare artifact dir: {}", err)))?;
    let tmp_path = temp_sibling_path(&path);

    let mut reader = File::open(source_path).await.map_err(|err| {
        ToolError::invalid_params(format!("sourcePath must be readable: {}", err))
    })?;
    let mut writer = File::create(&tmp_path)
        .await
        .map_err(|err| ToolError::internal(format!("Failed to create artifact: {}", err)))?;

    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    let mut truncated = false;

    loop {
        let n = reader
            .read(&mut buf)
            .await
            .map_err(|err| ToolError::internal(format!("Failed to read file: {}", err)))?;
        if n == 0 {
            break;
        }
        if let Some(limit) = max_bytes {
            if total + n as u64 > limit {
                let allowed = (limit - total) as usize;
                writer.write_all(&buf[..allowed]).await.map_err(|err| {
                    ToolError::internal(format!("Failed to write artifact: {}", err))
                })?;
                total += allowed as u64;
                truncated = true;
                break;
            }
        }
        writer
            .write_all(&buf[..n])
            .await
            .map_err(|err| ToolError::internal(format!("Failed to write artifact: {}", err)))?;
        total += n as u64;
    }
    writer.flush().await.ok();
    drop(writer);
    fs::rename(&tmp_path, &path)
        .await
        .map_err(|err| ToolError::internal(format!("Failed to finalize artifact: {}", err)))?;

    Ok(ArtifactInfo {
        uri: reference.uri.clone(),
        rel: reference.rel.clone(),
        path,
        bytes: total,
        truncated,
    })
}

fn random_id() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(12)
        .map(char::from)
        .collect()
}
