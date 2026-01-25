use crate::errors::ToolError;
use crate::utils::user_paths::expand_home_path;
use base64::Engine;
use serde_json::Value;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::{random_token, read_positive_int, LocalManager};

impl LocalManager {
    pub(super) async fn fs_read(&self, args: Value) -> Result<Value, ToolError> {
        let path = self.validation.ensure_string(
            args.get("path").unwrap_or(&Value::Null),
            "path",
            false,
        )?;
        let resolved = expand_home_path(&path);

        let offset = read_positive_int(args.get("offset")).unwrap_or(0);
        let length = read_positive_int(args.get("length")).unwrap_or(256 * 1024);
        let mut file = tokio::fs::File::open(&resolved)
            .await
            .map_err(|err| ToolError::invalid_params(format!("path must be readable: {}", err)))?;
        let metadata = file
            .metadata()
            .await
            .map_err(|err| ToolError::internal(err.to_string()))?;
        let file_bytes = metadata.len() as usize;
        let start = offset.min(file_bytes);
        let to_read = length.min(file_bytes.saturating_sub(start));
        let mut buffer = vec![0u8; to_read];
        if to_read > 0 {
            file.seek(std::io::SeekFrom::Start(start as u64))
                .await
                .map_err(|err| ToolError::internal(format!("Failed to seek file: {}", err)))?;
            file.read_exact(&mut buffer)
                .await
                .map_err(|err| ToolError::internal(format!("Failed to read file: {}", err)))?;
        }
        let encoding = args
            .get("encoding")
            .and_then(|v| v.as_str())
            .unwrap_or("utf8")
            .to_lowercase();
        let content = if encoding == "base64" {
            base64::engine::general_purpose::STANDARD.encode(&buffer)
        } else {
            String::from_utf8_lossy(&buffer).to_string()
        };
        Ok(serde_json::json!({
            "success": true,
            "path": resolved,
            "content": content,
            "encoding": encoding,
            "file_bytes": file_bytes,
            "offset": start,
            "length": to_read,
            "truncated": start + to_read < file_bytes,
        }))
    }

    pub(super) async fn fs_write(&self, args: Value) -> Result<Value, ToolError> {
        let path = self.validation.ensure_string(
            args.get("path").unwrap_or(&Value::Null),
            "path",
            false,
        )?;
        let resolved = expand_home_path(&path);
        let overwrite = args
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if resolved.exists() && !overwrite {
            return Err(ToolError::conflict("path already exists"));
        }

        let mode = args.get("mode").and_then(|v| v.as_i64()).unwrap_or(0o600);
        if !(0..=0o7777).contains(&mode) {
            return Err(ToolError::invalid_params(
                "mode must be a valid unix permission mask",
            ));
        }
        let mode = mode as u32;

        let encoding = args
            .get("encoding")
            .and_then(|v| v.as_str())
            .unwrap_or("utf8")
            .to_lowercase();

        let content = if let Some(raw) = args.get("content_base64").and_then(|v| v.as_str()) {
            base64::engine::general_purpose::STANDARD
                .decode(raw.as_bytes())
                .map_err(|_| ToolError::invalid_params("content_base64 is invalid"))?
        } else if let Some(text) = args.get("content").and_then(|v| v.as_str()) {
            if encoding == "base64" {
                base64::engine::general_purpose::STANDARD
                    .decode(text.as_bytes())
                    .map_err(|_| ToolError::invalid_params("content is not valid base64"))?
            } else {
                text.as_bytes().to_vec()
            }
        } else {
            return Err(ToolError::invalid_params(
                "content or content_base64 is required",
            ));
        };

        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| ToolError::internal(format!("Failed to create dir: {}", err)))?;
        }

        let tmp_path = resolved.with_file_name(format!(
            "{}.part-{}",
            resolved
                .file_name()
                .map(|v| v.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string()),
            random_token()
        ));

        if let Err(err) = tokio::fs::write(&tmp_path, &content).await {
            tokio::fs::remove_file(&tmp_path).await.ok();
            return Err(ToolError::internal(format!(
                "Failed to write file: {}",
                err
            )));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::Permissions::from_mode(mode);
            tokio::fs::set_permissions(&tmp_path, permissions)
                .await
                .ok();
        }

        let mut rename_result = tokio::fs::rename(&tmp_path, &resolved).await;
        if rename_result.is_err() && overwrite {
            let _ = tokio::fs::remove_file(&resolved).await;
            rename_result = tokio::fs::rename(&tmp_path, &resolved).await;
        }
        if let Err(err) = rename_result {
            tokio::fs::remove_file(&tmp_path).await.ok();
            return Err(ToolError::internal(format!(
                "Failed to replace file: {}",
                err
            )));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::Permissions::from_mode(mode);
            tokio::fs::set_permissions(&resolved, permissions)
                .await
                .ok();
        }

        Ok(serde_json::json!({
            "success": true,
            "path": resolved,
            "bytes": content.len(),
            "bytes_written": content.len(),
        }))
    }

    pub(super) async fn fs_list(&self, args: Value) -> Result<Value, ToolError> {
        let root = match args.get("path") {
            None | Some(Value::Null) => PathBuf::from("."),
            Some(value) => PathBuf::from(self.validation.ensure_string(value, "path", false)?),
        };
        let root = expand_home_path(root.to_string_lossy().as_ref());

        let recursive = args.get("recursive").and_then(|v| v.as_bool()) == Some(true);
        let max_depth = args
            .get("max_depth")
            .and_then(|v| v.as_i64())
            .map(|v| v.max(0) as usize)
            .unwrap_or(3);
        let with_stats = args.get("with_stats").and_then(|v| v.as_bool()) == Some(true);

        let mut entries = Vec::new();
        let mut stack = vec![(root.clone(), 0usize)];

        while let Some((current, depth)) = stack.pop() {
            let mut dir = tokio::fs::read_dir(&current).await.map_err(|err| {
                ToolError::invalid_params(format!("path must be a directory: {}", err))
            })?;

            while let Ok(Some(entry)) = dir.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                let full_path = entry.path();
                let file_type = entry.file_type().await.ok();

                let kind = if let Some(ft) = file_type.as_ref() {
                    if ft.is_dir() {
                        "dir"
                    } else if ft.is_file() {
                        "file"
                    } else if ft.is_symlink() {
                        "link"
                    } else {
                        "other"
                    }
                } else {
                    "unknown"
                };

                let mut item = serde_json::json!({
                    "path": full_path,
                    "name": name,
                    "type": kind,
                });

                if with_stats {
                    if let Ok(metadata) = tokio::fs::symlink_metadata(&full_path).await {
                        let size = metadata.len();
                        let mtime_ms = metadata
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_millis() as i64);
                        if let Some(obj) = item.as_object_mut() {
                            obj.insert("size".to_string(), serde_json::json!(size));
                            obj.insert("mtime_ms".to_string(), serde_json::json!(mtime_ms));
                        }
                    }
                }

                entries.push(item);

                let can_recurse = recursive
                    && depth < max_depth
                    && file_type.as_ref().map(|ft| ft.is_dir()).unwrap_or(false)
                    && !file_type
                        .as_ref()
                        .map(|ft| ft.is_symlink())
                        .unwrap_or(false);
                if can_recurse {
                    stack.push((full_path, depth + 1));
                }
            }
        }

        Ok(serde_json::json!({ "success": true, "path": root, "entries": entries }))
    }

    pub(super) async fn fs_stat(&self, args: Value) -> Result<Value, ToolError> {
        let path = self.validation.ensure_string(
            args.get("path").unwrap_or(&Value::Null),
            "path",
            false,
        )?;
        let resolved = expand_home_path(&path);
        let metadata = tokio::fs::symlink_metadata(&resolved)
            .await
            .map_err(|err| ToolError::invalid_params(format!("path must exist: {}", err)))?;
        let file_type = metadata.file_type();
        let kind = if file_type.is_dir() {
            "dir"
        } else if file_type.is_file() {
            "file"
        } else if file_type.is_symlink() {
            "link"
        } else {
            "other"
        };

        let mtime_ms = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);

        #[cfg(unix)]
        let mode: Option<u32> = {
            use std::os::unix::fs::PermissionsExt;
            Some(metadata.permissions().mode())
        };
        #[cfg(not(unix))]
        let mode: Option<u32> = None;

        Ok(serde_json::json!({
            "success": true,
            "path": resolved,
            "type": kind,
            "size": metadata.len(),
            "mode": mode,
            "mtime_ms": mtime_ms,
        }))
    }

    pub(super) async fn fs_mkdir(&self, args: Value) -> Result<Value, ToolError> {
        let path = self.validation.ensure_string(
            args.get("path").unwrap_or(&Value::Null),
            "path",
            false,
        )?;
        let resolved = expand_home_path(&path);
        let recursive = args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        if recursive {
            tokio::fs::create_dir_all(&resolved)
                .await
                .map_err(|err| ToolError::internal(format!("Failed to create dir: {}", err)))?;
        } else {
            tokio::fs::create_dir(&resolved)
                .await
                .map_err(|err| ToolError::internal(format!("Failed to create dir: {}", err)))?;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::Permissions::from_mode(0o700);
            tokio::fs::set_permissions(&resolved, permissions)
                .await
                .ok();
        }

        Ok(serde_json::json!({ "success": true, "path": resolved, "recursive": recursive }))
    }

    pub(super) async fn fs_rm(&self, args: Value) -> Result<Value, ToolError> {
        let path = self.validation.ensure_string(
            args.get("path").unwrap_or(&Value::Null),
            "path",
            false,
        )?;
        let resolved = expand_home_path(&path);
        let recursive = args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);

        let metadata = tokio::fs::symlink_metadata(&resolved).await;
        let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let is_symlink = metadata
            .as_ref()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);

        let result = if is_dir && !is_symlink {
            if recursive {
                tokio::fs::remove_dir_all(&resolved).await
            } else {
                tokio::fs::remove_dir(&resolved).await
            }
        } else {
            tokio::fs::remove_file(&resolved).await
        };

        if let Err(err) = result {
            if force && err.kind() == std::io::ErrorKind::NotFound {
                return Ok(serde_json::json!({
                    "success": true,
                    "path": resolved,
                    "recursive": recursive,
                    "force": force,
                }));
            }
            return Err(ToolError::internal(format!(
                "Failed to remove path: {}",
                err
            )));
        }

        Ok(serde_json::json!({
            "success": true,
            "path": resolved,
            "recursive": recursive,
            "force": force,
        }))
    }
}
