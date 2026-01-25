use crate::errors::ToolError;
use crate::utils::artifacts::{resolve_artifact_path, resolve_context_root};
use crate::utils::user_paths::expand_home_path;
use base64::Engine;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum StdinSource {
    Bytes(Vec<u8>),
    File(PathBuf),
}

pub fn resolve_stdin_source(args: &Value) -> Result<Option<StdinSource>, ToolError> {
    if let Some(path) = args.get("stdin_file").and_then(|v| v.as_str()) {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err(ToolError::invalid_params("stdin_file must not be empty"));
        }
        return Ok(Some(StdinSource::File(expand_home_path(trimmed))));
    }

    if let Some(raw) = args.get("stdin_ref").and_then(|v| v.as_str()) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ToolError::invalid_params("stdin_ref must not be empty"));
        }
        let rel = trimmed.trim_start_matches("artifact://");
        if rel.trim().is_empty() {
            return Err(ToolError::invalid_params("stdin_ref must include a path"));
        }
        let context_root = resolve_context_root().ok_or_else(|| {
            ToolError::denied("stdin_ref requires context repo root")
                .with_hint("Set INFRA_CONTEXT_REPO_ROOT or MCP_CONTEXT_REPO_ROOT.".to_string())
        })?;
        let path = resolve_artifact_path(&context_root, rel)?;
        if !path.exists() {
            return Err(ToolError::not_found(format!(
                "stdin_ref does not exist: {}",
                rel
            )));
        }
        return Ok(Some(StdinSource::File(path)));
    }

    if let Some(raw) = args.get("stdin_base64").and_then(|v| v.as_str()) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ToolError::invalid_params("stdin_base64 must not be empty"));
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(trimmed.as_bytes())
            .map_err(|err| {
                ToolError::invalid_params(format!("stdin_base64 decode failed: {}", err))
                    .with_hint("Provide standard base64-encoded bytes.".to_string())
            })?;
        return Ok(Some(StdinSource::Bytes(decoded)));
    }

    if let Some(text) = args.get("stdin").and_then(|v| v.as_str()) {
        return Ok(Some(StdinSource::Bytes(text.as_bytes().to_vec())));
    }

    Ok(None)
}

pub fn apply_stdin_source(map: &mut serde_json::Map<String, Value>, source: &StdinSource) {
    map.remove("stdin");
    map.remove("stdin_base64");
    map.remove("stdin_file");
    match source {
        StdinSource::Bytes(bytes) => {
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            map.insert("stdin_base64".to_string(), Value::String(encoded));
        }
        StdinSource::File(path) => {
            map.insert(
                "stdin_file".to_string(),
                Value::String(path.to_string_lossy().to_string()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn resolve_stdin_source_prefers_file() {
        let args = serde_json::json!({
            "stdin": "text",
            "stdin_base64": "dGVzdA==",
            "stdin_file": "~/stdin.txt"
        });
        let resolved = resolve_stdin_source(&args).unwrap();
        match resolved {
            Some(StdinSource::File(path)) => {
                assert!(path.to_string_lossy().contains("stdin.txt"));
            }
            _ => panic!("expected file stdin"),
        }
    }

    #[test]
    fn resolve_stdin_source_base64() {
        let args = serde_json::json!({ "stdin_base64": "dGVzdA==" });
        let resolved = resolve_stdin_source(&args).unwrap();
        match resolved {
            Some(StdinSource::Bytes(bytes)) => assert_eq!(bytes, b"test"),
            _ => panic!("expected bytes stdin"),
        }
    }

    #[test]
    fn resolve_stdin_source_ref() {
        let root = std::env::temp_dir().join(format!("infra-stdin-{}", uuid::Uuid::new_v4()));
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&artifacts).unwrap();
        let rel = "stdin/ref.txt";
        let path = artifacts.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"ok").unwrap();
        std::env::set_var("INFRA_CONTEXT_REPO_ROOT", &root);

        let args = serde_json::json!({ "stdin_ref": format!("artifact://{}", rel) });
        let resolved = resolve_stdin_source(&args).unwrap();
        match resolved {
            Some(StdinSource::File(found)) => {
                assert!(found.ends_with(rel));
            }
            _ => panic!("expected file stdin"),
        }

        let _ = fs::remove_dir_all(&root);
        std::env::remove_var("INFRA_CONTEXT_REPO_ROOT");
    }
}
