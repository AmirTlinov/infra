use crate::errors::ToolError;
use crate::utils::bundled_manifests::{bundled_runbooks_json, BUNDLED_RUNBOOKS_MANIFEST_URI};
use crate::utils::effects::resolve_effects;
use crate::utils::listing::ListFilters;
use crate::utils::paths::{resolve_default_runbooks_path, resolve_runbooks_path};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const RUNBOOK_SOURCE: &str = "manifest";
const FILE_BACKED_MANIFEST_SOURCE: &str = "file_backed_manifest";
const BUNDLED_MANIFEST_SOURCE: &str = "bundled_manifest";
const UNCONFIGURED_MANIFEST_SOURCE: &str = "unconfigured_manifest";

#[derive(Clone)]
pub struct RunbookService {
    default_manifest_path: Option<PathBuf>,
    manifest_path: PathBuf,
    manifest: RunbookManifest,
}

#[derive(Clone)]
struct RunbookManifest {
    path: Option<PathBuf>,
    source: String,
    version: Option<Value>,
    sha256: Option<String>,
}

impl RunbookService {
    pub fn new() -> Result<Self, ToolError> {
        let default_manifest_path = resolve_default_runbooks_path();
        let manifest_path = resolve_runbooks_path();
        let manifest = load_runbook_manifest(
            default_manifest_path.as_deref(),
            Some(manifest_path.as_path()),
        )?;
        Ok(Self {
            default_manifest_path,
            manifest_path,
            manifest,
        })
    }

    pub fn manifest_path(&self) -> &Path {
        self.manifest_path.as_path()
    }

    pub fn manifest_metadata(&self) -> Value {
        serde_json::json!({
            "manifest_path": self.manifest_path_value(),
            "manifest_source": self.manifest.source.clone(),
            "manifest_version": self.manifest.version.clone().unwrap_or(Value::Null),
            "manifest_sha256": self.manifest_sha256_value(),
        })
    }

    fn manifest_path_value(&self) -> Value {
        self.manifest
            .path
            .as_ref()
            .map(|path| Value::String(path.to_string_lossy().to_string()))
            .unwrap_or(Value::Null)
    }

    fn manifest_sha256_value(&self) -> Value {
        self.manifest
            .sha256
            .as_ref()
            .map(|sha| Value::String(sha.clone()))
            .unwrap_or(Value::Null)
    }

    fn merge_bundled_manifest(
        merged: &mut HashMap<String, (Value, String)>,
    ) -> Result<(), ToolError> {
        merge_bundled_manifest(merged).map(|_| ())
    }

    fn merge_manifest(
        merged: &mut HashMap<String, (Value, String)>,
        path: Option<&Path>,
        source: &str,
    ) -> Result<bool, ToolError> {
        Ok(merge_manifest(merged, path, source)?.is_some())
    }

    fn manifest_runbooks(&self) -> Result<HashMap<String, (Value, String)>, ToolError> {
        let mut merged = HashMap::new();
        let default_path = self.default_manifest_path.as_deref();
        let project_path = Some(self.manifest_path.as_path());
        let same_manifest = default_path == project_path;

        if same_manifest {
            let loaded_manifest = Self::merge_manifest(&mut merged, project_path, "manifest")?;
            if !loaded_manifest {
                Self::merge_bundled_manifest(&mut merged)?;
            }
            return Ok(merged);
        }

        let loaded_default = Self::merge_manifest(&mut merged, default_path, "default_manifest")?;
        if !loaded_default {
            Self::merge_bundled_manifest(&mut merged)?;
        }
        Self::merge_manifest(&mut merged, project_path, "manifest")?;
        Ok(merged)
    }

    fn validate_runbook(runbook: &Value) -> Result<(), ToolError> {
        let obj = runbook
            .as_object()
            .ok_or_else(|| ToolError::invalid_params("runbook must be an object"))?;
        let empty_steps = Vec::new();
        let steps = obj
            .get("steps")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty_steps);
        if steps.is_empty() {
            return Err(
                ToolError::invalid_params("runbook.steps must be a non-empty array").with_hint(
                    "Provide at least one step: [{ tool: \"ssh\", args: { ... } }].".to_string(),
                ),
            );
        }
        Ok(())
    }

    fn compatibility_only_error(&self, action: &str) -> ToolError {
        ToolError::invalid_params(format!(
            "{} is compatibility-only and no longer supported in normal mode",
            action
        ))
        .with_hint(format!(
            "Edit the manifest-backed runbook definition in {} and execute it by name.",
            self.manifest_path.display()
        ))
        .with_details(serde_json::json!({
            "stage": "compatibility_runbook_mutation",
            "action": action,
            "manifest_path": self.manifest_path.display().to_string(),
        }))
    }

    pub fn set_runbook(&self, _name: &str, _runbook: &Value) -> Result<Value, ToolError> {
        Err(self.compatibility_only_error("runbook_upsert"))
    }

    pub fn resolve_runbook(&self, name: &str) -> Result<Value, ToolError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ToolError::invalid_params(
                "runbook name must be a non-empty string",
            ));
        }
        let (entry, source) = self.manifest_runbooks()?.remove(name).ok_or_else(|| {
            ToolError::not_found(format!("runbook '{}' not found", name)).with_hint(
                "Use action=runbook_list to see known manifest-backed runbooks.".to_string(),
            )
        })?;
        let mut map = entry.as_object().cloned().unwrap_or_default();
        map.insert("name".to_string(), Value::String(name.to_string()));
        map.insert("source".to_string(), Value::String(source));
        Ok(Value::Object(map))
    }

    pub fn get_runbook(&self, name: &str) -> Result<Value, ToolError> {
        Ok(serde_json::json!({"success": true, "runbook": self.resolve_runbook(name)?}))
    }

    pub fn list_runbooks(&self, filters: &ListFilters) -> Result<Value, ToolError> {
        let merged = self.manifest_runbooks()?;
        let mut names: Vec<String> = merged.keys().cloned().collect();
        names.sort();
        let mut items = Vec::new();
        for name in names {
            let Some((runbook, source)) = merged.get(&name) else {
                continue;
            };
            let steps_len = runbook
                .get("steps")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len())
                .unwrap_or(0);
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String(name.clone()));
            if let Some(desc) = runbook.get("description") {
                if !desc.is_null() {
                    map.insert("description".to_string(), desc.clone());
                }
            }
            let tags = runbook
                .get("tags")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            map.insert("tags".to_string(), Value::Array(tags));
            map.insert("effects".to_string(), resolve_effects(runbook).to_value());
            if let Some(when) = runbook.get("when") {
                map.insert("when".to_string(), when.clone());
            }
            if let Some(inputs) = runbook.get("inputs") {
                if !inputs.is_null() {
                    map.insert("inputs".to_string(), inputs.clone());
                }
            }
            map.insert(
                "steps".to_string(),
                Value::Number(serde_json::Number::from(steps_len as i64)),
            );
            if let Some(created_at) = runbook.get("created_at") {
                if !created_at.is_null() {
                    map.insert("created_at".to_string(), created_at.clone());
                }
            }
            if let Some(updated_at) = runbook.get("updated_at") {
                if !updated_at.is_null() {
                    map.insert("updated_at".to_string(), updated_at.clone());
                }
            }
            map.insert("source".to_string(), Value::String(source.clone()));
            if let Some(manifest_source) = runbook.get("manifest_source") {
                if !manifest_source.is_null() {
                    map.insert("manifest_source".to_string(), manifest_source.clone());
                }
            }
            if let Some(manifest_path) = runbook.get("manifest_path") {
                if !manifest_path.is_null() {
                    map.insert("manifest_path".to_string(), manifest_path.clone());
                }
            }
            if let Some(manifest_version) = runbook.get("manifest_version") {
                map.insert("manifest_version".to_string(), manifest_version.clone());
            }
            if let Some(manifest_sha256) = runbook.get("manifest_sha256") {
                if !manifest_sha256.is_null() {
                    map.insert("manifest_sha256".to_string(), manifest_sha256.clone());
                }
            }
            items.push(Value::Object(map));
        }
        let result = filters.apply(items, &["name", "description", "tags"], Some("tags"));
        Ok(serde_json::json!({
            "success": true,
            "runbooks": result.items,
            "meta": filters.meta(result.total, result.items.len()),
        }))
    }

    pub fn delete_runbook(&self, _name: &str) -> Result<Value, ToolError> {
        Err(self.compatibility_only_error("runbook_delete"))
    }
}

#[derive(Clone)]
struct ManifestInfo {
    path: PathBuf,
    source: String,
    version: Option<Value>,
    sha256: String,
}

fn load_runbook_manifest(
    default_path: Option<&Path>,
    manifest_path: Option<&Path>,
) -> Result<RunbookManifest, ToolError> {
    let mut merged = HashMap::new();
    let same_manifest = default_path == manifest_path;

    let primary = if same_manifest {
        let manifest_meta = merge_manifest(&mut merged, manifest_path, "manifest")?;
        let bundled_meta = if manifest_meta.is_none() {
            merge_bundled_manifest(&mut merged)?
        } else {
            None
        };
        manifest_meta.or(bundled_meta)
    } else {
        let default_meta = merge_manifest(&mut merged, default_path, "default_manifest")?;
        let bundled_meta = if default_meta.is_none() {
            merge_bundled_manifest(&mut merged)?
        } else {
            None
        };
        let manifest_meta = merge_manifest(&mut merged, manifest_path, "manifest")?;
        manifest_meta.or(default_meta).or(bundled_meta)
    };

    let (path, version, sha256) = if let Some(meta) = primary.as_ref() {
        (
            Some(meta.path.clone()),
            meta.version.clone(),
            Some(meta.sha256.clone()),
        )
    } else {
        (
            manifest_path
                .or(default_path)
                .map(|value| value.to_path_buf()),
            None,
            None,
        )
    };

    Ok(RunbookManifest {
        path,
        source: primary
            .as_ref()
            .map(|meta| meta.source.clone())
            .unwrap_or_else(|| UNCONFIGURED_MANIFEST_SOURCE.to_string()),
        version,
        sha256,
    })
}

fn merge_bundled_manifest(
    merged: &mut HashMap<String, (Value, String)>,
) -> Result<Option<ManifestInfo>, ToolError> {
    let (runbooks, info) = read_runbooks_map_from_str(
        bundled_runbooks_json(),
        BUNDLED_MANIFEST_SOURCE,
        BUNDLED_RUNBOOKS_MANIFEST_URI,
    )?;
    for (name, runbook) in runbooks {
        merged.insert(name, (runbook, BUNDLED_MANIFEST_SOURCE.to_string()));
    }
    Ok(Some(info))
}

fn merge_manifest(
    merged: &mut HashMap<String, (Value, String)>,
    path: Option<&Path>,
    source: &str,
) -> Result<Option<ManifestInfo>, ToolError> {
    let Some(path) = path else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let (runbooks, info) = read_runbooks_map(path, source)?;
    for (name, runbook) in runbooks {
        merged.insert(name, (runbook, source.to_string()));
    }
    Ok(Some(info))
}

fn read_runbooks_map(
    path: &Path,
    source: &str,
) -> Result<(HashMap<String, Value>, ManifestInfo), ToolError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| ToolError::internal(format!("Failed to load runbooks: {}", err)))?;
    read_runbooks_map_from_str(&raw, source, path.to_string_lossy().as_ref())
}

fn read_runbooks_map_from_str(
    raw: &str,
    source: &str,
    manifest_path: &str,
) -> Result<(HashMap<String, Value>, ManifestInfo), ToolError> {
    let parsed: Value = serde_json::from_str(raw)
        .map_err(|err| ToolError::internal(format!("Failed to parse runbooks: {}", err)))?;
    let manifest_version = parsed.get("version").cloned();
    let manifest_sha256 = format!("{:x}", Sha256::digest(raw.as_bytes()));
    let entries = parsed.get("runbooks").cloned().unwrap_or_else(|| {
        parsed
            .as_object()
            .cloned()
            .map(|mut obj| {
                obj.remove("version");
                Value::Object(obj)
            })
            .unwrap_or(Value::Null)
    });
    let runbooks = entries
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect::<HashMap<_, _>>();
    for (name, runbook) in &runbooks {
        RunbookService::validate_runbook(runbook).map_err(|err| {
            ToolError::invalid_params(format!(
                "Runbook manifest '{}' has invalid entry '{}': {}",
                manifest_path,
                name,
                err.message
            ))
            .with_hint(
                "Fix the manifest entry and retry; normal-mode runbook execution is manifest-backed."
                    .to_string(),
            )
        })?;
    }

    let runbooks = runbooks
        .into_iter()
        .map(|(name, runbook)| {
            (
                name.clone(),
                inject_manifest_metadata(
                    runbook,
                    &name,
                    source,
                    manifest_path,
                    manifest_version.as_ref(),
                    &manifest_sha256,
                ),
            )
        })
        .collect::<HashMap<_, _>>();

    Ok((
        runbooks,
        ManifestInfo {
            path: PathBuf::from(manifest_path),
            source: match source {
                "manifest" | "default_manifest" => FILE_BACKED_MANIFEST_SOURCE.to_string(),
                BUNDLED_MANIFEST_SOURCE => BUNDLED_MANIFEST_SOURCE.to_string(),
                other => other.to_string(),
            },
            version: manifest_version,
            sha256: manifest_sha256,
        },
    ))
}

fn inject_manifest_metadata(
    entry: Value,
    name: &str,
    source: &str,
    manifest_path: &str,
    manifest_version: Option<&Value>,
    manifest_sha256: &str,
) -> Value {
    let mut payload = entry.as_object().cloned().unwrap_or_default();
    payload.insert("name".to_string(), Value::String(name.to_string()));
    payload.insert(
        "source".to_string(),
        Value::String(RUNBOOK_SOURCE.to_string()),
    );
    payload.insert(
        "manifest_source".to_string(),
        Value::String(match source {
            "manifest" | "default_manifest" => FILE_BACKED_MANIFEST_SOURCE.to_string(),
            BUNDLED_MANIFEST_SOURCE => BUNDLED_MANIFEST_SOURCE.to_string(),
            other => other.to_string(),
        }),
    );
    payload.insert(
        "manifest_path".to_string(),
        Value::String(manifest_path.to_string()),
    );
    payload.insert(
        "manifest_version".to_string(),
        manifest_version.cloned().unwrap_or(Value::Null),
    );
    payload.insert(
        "manifest_sha256".to_string(),
        Value::String(manifest_sha256.to_string()),
    );
    payload
        .entry("source".to_string())
        .or_insert_with(|| Value::String(RUNBOOK_SOURCE.to_string()));
    Value::Object(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn load_runbook_manifest_prefers_project_manifest_metadata_over_bundled_fallback() {
        let tmp_root =
            std::env::temp_dir().join(format!("infra-runbook-manifest-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_root).expect("create tmp");
        let project_manifest = tmp_root.join("runbooks.json");
        fs::write(
            &project_manifest,
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": 7,
                "runbooks": {
                    "demo.observe": {
                        "steps": [
                            { "tool": "state", "args": { "action": "get", "key": "demo", "scope": "session" } }
                        ]
                    }
                }
            }))
            .expect("serialize project runbooks"),
        )
        .expect("write project runbooks");

        let manifest =
            load_runbook_manifest(None, Some(project_manifest.as_path())).expect("load manifest");

        assert_eq!(manifest.source, FILE_BACKED_MANIFEST_SOURCE);
        assert_eq!(manifest.path, Some(project_manifest.clone()));
        assert_eq!(manifest.version, Some(serde_json::json!(7)));
        assert!(manifest.sha256.is_some());
    }
}
