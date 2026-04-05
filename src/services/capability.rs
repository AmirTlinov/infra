use crate::errors::ToolError;
use crate::services::security::Security;
use crate::utils::bundled_manifests::{
    bundled_capabilities_json, BUNDLED_CAPABILITIES_MANIFEST_URI,
};
use crate::utils::paths::{resolve_capabilities_path, resolve_default_capabilities_path};
use crate::utils::when_matcher::matches_when;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

const CAPABILITY_SOURCE: &str = "manifest";
const FILE_BACKED_MANIFEST_SOURCE: &str = "file_backed_manifest";
const BUNDLED_MANIFEST_SOURCE: &str = "bundled_manifest";
const UNCONFIGURED_MANIFEST_SOURCE: &str = "unconfigured_manifest";

#[derive(Clone)]
pub struct CapabilityService {
    _security: Arc<Security>,
    manifest_path: PathBuf,
    manifest: Arc<CapabilityManifest>,
}

#[derive(Clone)]
struct CapabilityManifest {
    path: Option<PathBuf>,
    source: String,
    version: Option<Value>,
    sha256: Option<String>,
    capabilities: HashMap<String, Value>,
}

impl CapabilityService {
    pub fn new(security: Arc<Security>) -> Result<Self, ToolError> {
        let default_path = resolve_default_capabilities_path();
        let manifest_path = resolve_capabilities_path();
        let manifest = Arc::new(load_capability_manifest(
            default_path.as_deref(),
            Some(manifest_path.as_path()),
        )?);
        Ok(Self {
            _security: security,
            manifest_path,
            manifest,
        })
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

    fn apply_manifest_provenance(&self, map: &mut serde_json::Map<String, Value>) {
        map.entry("source".to_string())
            .or_insert_with(|| Value::String(CAPABILITY_SOURCE.to_string()));
        map.entry("manifest_path".to_string())
            .or_insert_with(|| self.manifest_path_value());
        map.entry("manifest_source".to_string())
            .or_insert_with(|| Value::String(self.manifest.source.clone()));
        map.entry("manifest_version".to_string())
            .or_insert_with(|| self.manifest.version.clone().unwrap_or(Value::Null));
        map.entry("manifest_sha256".to_string())
            .or_insert_with(|| self.manifest_sha256_value());
    }

    fn hydrate_capability(&self, name: &str, capability: &Value) -> Value {
        let mut entry = capability.as_object().cloned().unwrap_or_default();
        entry.insert("name".to_string(), Value::String(name.to_string()));
        entry
            .entry("depends_on".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        self.apply_manifest_provenance(&mut entry);
        Value::Object(entry)
    }

    fn compatibility_mutation_error(&self, action: &str, name: &str) -> ToolError {
        let manifest_hint = format!("Edit {} directly and retry.", self.manifest_path.display());

        ToolError::invalid_params(format!(
            "capability action '{}' is compatibility-only and no longer supported in manifest-first normal mode",
            action
        ))
        .with_hint(manifest_hint)
        .with_details(serde_json::json!({
            "stage": "compatibility_capability_mutation",
            "action": action,
            "name": name,
            "manifest_path": self.manifest_path.display().to_string(),
            "manifest_source": self.manifest.source.clone(),
            "manifest_version": self.manifest.version.clone().unwrap_or(Value::Null),
            "manifest_sha256": self.manifest_sha256_value(),
        }))
    }

    pub fn list_capabilities(&self) -> Result<Value, ToolError> {
        let mut out = Vec::new();
        let mut names: Vec<String> = self.manifest.capabilities.keys().cloned().collect();
        names.sort();
        for name in names {
            let cap = self.manifest.capabilities.get(&name).ok_or_else(|| {
                ToolError::internal("Capability disappeared while listing".to_string())
            })?;
            out.push(self.hydrate_capability(&name, cap));
        }
        Ok(Value::Array(out))
    }

    pub fn get_capability(&self, name: &str) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "Capability name must be a non-empty string",
            ));
        }
        let entry = self.manifest.capabilities.get(name).ok_or_else(|| {
            ToolError::not_found(format!("Capability '{}' not found", name))
                .with_hint("Use action=capability_list to see known capabilities.".to_string())
        })?;
        Ok(self.hydrate_capability(name, entry))
    }

    pub fn find_all_by_intent(&self, intent_type: &str) -> Result<Vec<Value>, ToolError> {
        if intent_type.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "Intent type must be a non-empty string",
            ));
        }
        let mut matches = Vec::new();
        if let Some(entry) = self.manifest.capabilities.get(intent_type) {
            matches.push(self.hydrate_capability(intent_type, entry));
        }
        for (name, cap) in &self.manifest.capabilities {
            if name == intent_type {
                continue;
            }
            if cap.get("intent").and_then(|v| v.as_str()) == Some(intent_type) {
                matches.push(self.hydrate_capability(name, cap));
            }
        }
        Ok(matches)
    }

    pub fn resolve_by_intent(
        &self,
        intent_type: &str,
        context: Option<&Value>,
    ) -> Result<Value, ToolError> {
        let candidates = self.find_all_by_intent(intent_type)?;
        if candidates.is_empty() {
            return Err(ToolError::not_found(format!(
                "Capability for intent '{}' not found",
                intent_type
            ))
            .with_hint(
                "Check capabilities.json (or configure capability mappings) and retry.".to_string(),
            )
            .with_details(serde_json::json!({"intent_type": intent_type})));
        }

        let context_value = context
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let mut matched: Vec<Value> = candidates
            .into_iter()
            .filter(|candidate| {
                matches_when(
                    candidate.get("when").unwrap_or(&Value::Null),
                    &context_value,
                )
            })
            .collect();
        if matched.is_empty() {
            return Err(ToolError::not_found(format!(
                "No capability matched when-clause for intent '{}'",
                intent_type
            ))
            .with_hint(
                "Provide the required context inputs (project/target/repo_root/etc) or adjust capability.when clauses."
                    .to_string(),
            )
            .with_details(serde_json::json!({"intent_type": intent_type})));
        }
        matched.sort_by(|a, b| {
            let a_name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let b_name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let a_direct = if a_name == intent_type { 0 } else { 1 };
            let b_direct = if b_name == intent_type { 0 } else { 1 };
            if a_direct != b_direct {
                return a_direct.cmp(&b_direct);
            }
            a_name.cmp(b_name)
        });
        if matched.len() > 1 {
            return Err(ambiguity_error(
                format!("Intent '{}' matched multiple capabilities", intent_type),
                matched
                    .iter()
                    .map(|candidate| {
                        serde_json::json!({
                            "name": candidate.get("name").cloned().unwrap_or(Value::Null),
                            "intent": candidate.get("intent").cloned().unwrap_or(Value::Null),
                            "manifest_source": candidate.get("manifest_source").cloned().unwrap_or(Value::Null),
                            "manifest_path": candidate.get("manifest_path").cloned().unwrap_or(Value::Null),
                            "when": candidate.get("when").cloned().unwrap_or(Value::Null),
                            "reason": "matched intent and when-clause"
                        })
                    })
                    .collect(),
            ));
        }
        Ok(matched[0].clone())
    }

    pub fn resolve_for_operation(
        &self,
        family: &str,
        verb: &str,
        context: Option<&Value>,
    ) -> Result<Value, ToolError> {
        let family = family.trim().to_lowercase();
        let verb = verb.trim().to_lowercase();
        if family.is_empty() {
            return Err(ToolError::invalid_params(
                "Operation family must be a non-empty string",
            ));
        }
        if verb.is_empty() {
            return Err(ToolError::invalid_params(
                "Operation verb must be a non-empty string",
            ));
        }

        let context_value = context
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let mut scored = Vec::new();
        for (name, capability) in &self.manifest.capabilities {
            if !matches_when(
                capability.get("when").unwrap_or(&Value::Null),
                &context_value,
            ) {
                continue;
            }
            let score = operation_score(capability, &family, &verb);
            if score > 0 {
                scored.push((
                    score,
                    name.clone(),
                    self.hydrate_capability(name, capability),
                ));
            }
        }

        if scored.is_empty() {
            return Err(ToolError::not_found(format!(
                "No capability matched operation family='{}' verb='{}'",
                family, verb
            ))
            .with_hint(
                "Provide an explicit capability or intent, or inspect infra://capabilities/families."
                    .to_string(),
            )
            .with_details(serde_json::json!({
                "family": family,
                "verb": verb,
            })));
        }

        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let top_score = scored[0].0;
        let top_matches = scored
            .iter()
            .filter(|(score, _, _)| *score == top_score)
            .collect::<Vec<_>>();
        if top_matches.len() > 1 {
            return Err(ambiguity_error(
                format!(
                    "Operation family='{}' verb='{}' matched multiple capabilities",
                    family, verb
                ),
                top_matches
                    .iter()
                    .map(|(score, name, capability)| {
                        serde_json::json!({
                            "name": name,
                            "score": score,
                            "intent": capability.get("intent").cloned().unwrap_or(Value::Null),
                            "manifest_source": capability.get("manifest_source").cloned().unwrap_or(Value::Null),
                            "manifest_path": capability.get("manifest_path").cloned().unwrap_or(Value::Null),
                            "reason": operation_reason(capability, &family, &verb),
                        })
                    })
                    .collect(),
            ));
        }
        Ok(scored[0].2.clone())
    }

    pub fn families_index(&self) -> Result<Value, ToolError> {
        let mut families: HashMap<String, serde_json::Map<String, Value>> = HashMap::new();

        for (name, capability) in &self.manifest.capabilities {
            let family = capability_family(capability);
            let verb = capability_verb(capability);
            let intent = capability
                .get("intent")
                .and_then(|v| v.as_str())
                .unwrap_or(name)
                .to_string();

            let entry = families.entry(family.clone()).or_insert_with(|| {
                let mut obj = serde_json::Map::new();
                obj.insert("family".to_string(), Value::String(family.clone()));
                obj.insert("verbs".to_string(), Value::Array(Vec::new()));
                obj.insert("intents".to_string(), Value::Array(Vec::new()));
                obj.insert("capabilities".to_string(), Value::Array(Vec::new()));
                self.apply_manifest_provenance(&mut obj);
                obj
            });

            if let Some(arr) = entry.get_mut("verbs").and_then(|v| v.as_array_mut()) {
                push_unique(arr, Value::String(verb));
            }
            if let Some(arr) = entry.get_mut("intents").and_then(|v| v.as_array_mut()) {
                push_unique(arr, Value::String(intent));
            }
            if let Some(arr) = entry.get_mut("capabilities").and_then(|v| v.as_array_mut()) {
                push_unique(arr, Value::String(name.clone()));
            }
        }

        let mut values = families
            .into_values()
            .map(Value::Object)
            .collect::<Vec<_>>();
        values.sort_by(|a, b| {
            a.get("family")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .cmp(b.get("family").and_then(|v| v.as_str()).unwrap_or(""))
        });
        Ok(Value::Array(values))
    }

    pub fn set_capability(&self, name: &str, _config: &Value) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "Capability name must be a non-empty string",
            ));
        }
        Err(self.compatibility_mutation_error("set", name))
    }

    pub fn delete_capability(&self, name: &str) -> Result<Value, ToolError> {
        if name.trim().is_empty() {
            return Err(ToolError::invalid_params(
                "Capability name must be a non-empty string",
            ));
        }
        Err(self.compatibility_mutation_error("delete", name))
    }
}

fn load_capability_manifest(
    default_path: Option<&Path>,
    manifest_path: Option<&Path>,
) -> Result<CapabilityManifest, ToolError> {
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

    Ok(CapabilityManifest {
        path,
        source: primary
            .as_ref()
            .map(|meta| meta.source.clone())
            .unwrap_or_else(|| UNCONFIGURED_MANIFEST_SOURCE.to_string()),
        version,
        sha256,
        capabilities: merged,
    })
}

#[derive(Clone)]
struct ManifestInfo {
    path: PathBuf,
    source: String,
    version: Option<Value>,
    sha256: String,
}

fn merge_manifest(
    merged: &mut HashMap<String, Value>,
    path: Option<&Path>,
    source: &str,
) -> Result<Option<ManifestInfo>, ToolError> {
    let Some(path) = path else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let (capabilities, info) = read_capabilities_map(path, source)?;
    for (name, capability) in capabilities {
        merged.insert(name, capability);
    }
    Ok(Some(info))
}

fn merge_bundled_manifest(
    merged: &mut HashMap<String, Value>,
) -> Result<Option<ManifestInfo>, ToolError> {
    let (capabilities, info) = read_capabilities_map_from_str(
        bundled_capabilities_json(),
        BUNDLED_MANIFEST_SOURCE,
        BUNDLED_CAPABILITIES_MANIFEST_URI,
    )?;
    for (name, capability) in capabilities {
        merged.insert(name, capability);
    }
    Ok(Some(info))
}

fn read_capabilities_map(
    path: &Path,
    source: &str,
) -> Result<(HashMap<String, Value>, ManifestInfo), ToolError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| ToolError::internal(format!("Failed to read capabilities: {}", err)))?;
    read_capabilities_map_from_str(&raw, source, path.to_string_lossy().as_ref())
}

fn read_capabilities_map_from_str(
    raw: &str,
    source: &str,
    manifest_path: &str,
) -> Result<(HashMap<String, Value>, ManifestInfo), ToolError> {
    let parsed: Value = serde_json::from_str(raw)
        .map_err(|err| ToolError::internal(format!("Failed to parse capabilities: {}", err)))?;
    let manifest_version = parsed.get("version").cloned();
    let manifest_sha256 = format!("{:x}", Sha256::digest(raw.as_bytes()));
    let entries = parsed.get("capabilities").cloned().unwrap_or_else(|| {
        parsed
            .as_object()
            .cloned()
            .map(|mut obj| {
                obj.remove("version");
                Value::Object(obj)
            })
            .unwrap_or(Value::Null)
    });
    let mut capabilities = HashMap::new();

    match entries {
        Value::Array(list) => {
            for entry in list {
                let name = entry
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|value| value.to_string());
                if let Some(name) = name {
                    capabilities.insert(
                        name.clone(),
                        inject_manifest_metadata(
                            entry,
                            &name,
                            source,
                            manifest_path,
                            manifest_version.as_ref(),
                            &manifest_sha256,
                        ),
                    );
                }
            }
        }
        Value::Object(map) => {
            for (name, entry) in map {
                capabilities.insert(
                    name.clone(),
                    inject_manifest_metadata(
                        entry,
                        &name,
                        source,
                        manifest_path,
                        manifest_version.as_ref(),
                        &manifest_sha256,
                    ),
                );
            }
        }
        _ => {}
    }

    Ok((
        capabilities,
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
    payload.insert("source".to_string(), Value::String(source.to_string()));
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
        .entry("depends_on".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    Value::Object(payload)
}

fn push_unique(target: &mut Vec<Value>, value: Value) {
    if target.iter().any(|existing| existing == &value) {
        return;
    }
    target.push(value);
    target.sort_by_key(|item| item.to_string());
}

fn capability_family(capability: &Value) -> String {
    let name = capability
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let intent = capability
        .get("intent")
        .and_then(|v| v.as_str())
        .unwrap_or(name.as_str())
        .to_lowercase();
    token_before_dot(&intent)
        .or_else(|| token_before_dot(&name))
        .unwrap_or_else(|| intent.clone())
}

fn capability_verb(capability: &Value) -> String {
    let name = capability
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let intent = capability
        .get("intent")
        .and_then(|v| v.as_str())
        .unwrap_or(name.as_str())
        .to_lowercase();

    token_after_dot(&intent)
        .or_else(|| token_after_dot(&name))
        .or_else(|| {
            if capability
                .get("effects")
                .and_then(|v| v.get("requires_apply"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                Some("apply".to_string())
            } else {
                Some("observe".to_string())
            }
        })
        .unwrap_or_else(|| "observe".to_string())
}

fn token_before_dot(value: &str) -> Option<String> {
    value
        .split('.')
        .next()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| token.to_string())
}

fn token_after_dot(value: &str) -> Option<String> {
    let mut parts = value
        .split('.')
        .map(str::trim)
        .filter(|token| !token.is_empty());
    let _family = parts.next()?;
    let rest = parts.collect::<Vec<_>>();
    if rest.is_empty() {
        None
    } else {
        Some(rest.join("."))
    }
}

fn operation_score(capability: &Value, family: &str, verb: &str) -> i32 {
    let name = capability
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let intent = capability
        .get("intent")
        .and_then(|v| v.as_str())
        .unwrap_or(name.as_str())
        .to_lowercase();

    let belongs_to_family = intent == family
        || intent.starts_with(&(family.to_string() + "."))
        || name.starts_with(&(family.to_string() + "."));
    if !belongs_to_family {
        return 0;
    }

    let aliases = operation_aliases(verb);
    let family_prefix = format!("{}.", family);

    for alias in &aliases {
        let exact_intent = format!("{}{}", family_prefix, alias);
        if intent == exact_intent {
            return 100;
        }
        if name == exact_intent {
            return 95;
        }
    }

    let tokens = capability_tokens(&intent, &name, family);
    let mut best = if intent == family {
        base_score_for_family_root(capability, verb)
    } else {
        10
    };
    for alias in &aliases {
        if tokens.iter().any(|token| token == alias) {
            best = best.max(80);
        } else if tokens.iter().any(|token| token.contains(alias)) {
            best = best.max(70);
        }
    }
    best
}

fn capability_tokens(intent: &str, name: &str, family: &str) -> Vec<String> {
    let mut out = Vec::new();
    for source in [intent, name] {
        let mut parts = source
            .split('.')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(|token| token.to_lowercase())
            .collect::<Vec<_>>();
        if parts.first().map(|token| token == family).unwrap_or(false) {
            parts.remove(0);
        }
        for token in parts {
            if !out.contains(&token) {
                out.push(token);
            }
        }
    }
    out
}

fn base_score_for_family_root(capability: &Value, verb: &str) -> i32 {
    let kind = capability
        .get("effects")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("read");
    let requires_apply = capability
        .get("effects")
        .and_then(|v| v.get("requires_apply"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    match verb {
        "apply" if requires_apply || kind == "write" || kind == "mixed" => 60,
        "rollback" if requires_apply || kind == "write" || kind == "mixed" => 60,
        "observe" | "verify" if !requires_apply && kind == "read" => 50,
        "plan" if !requires_apply && kind == "read" => 45,
        _ => 20,
    }
}

fn operation_aliases(verb: &str) -> Vec<String> {
    match verb {
        "observe" => vec![
            "observe",
            "status",
            "inspect",
            "snapshot",
            "triage",
            "health",
            "list",
            "system_info",
            "render",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        "plan" => vec!["plan", "diff", "render", "propose"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        "apply" => vec!["apply", "sync", "deploy", "release"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        "verify" => vec!["verify", "status", "inspect", "health", "baseline"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        "rollback" => vec!["rollback", "revert", "undo"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        _ => vec![verb.to_string()],
    }
}

fn ambiguity_error(message: String, candidates: Vec<Value>) -> ToolError {
    ToolError::new(
        crate::errors::ToolErrorKind::Conflict,
        "AMBIGUOUS_CAPABILITY",
        message,
    )
    .with_hint(
        "Provide an explicit capability, or tighten project/target/repo_root context so only one candidate remains."
            .to_string(),
    )
    .with_details(serde_json::json!({
        "candidates": candidates,
    }))
}

fn operation_reason(capability: &Value, family: &str, verb: &str) -> String {
    let tokens = capability_tokens(
        capability
            .get("intent")
            .and_then(|value| value.as_str())
            .unwrap_or(""),
        capability
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or(""),
        family,
    );
    let aliases = operation_aliases(verb);
    let matched_aliases = aliases
        .iter()
        .filter(|alias| {
            tokens
                .iter()
                .any(|token| token == *alias || token.contains(*alias))
        })
        .cloned()
        .collect::<Vec<_>>();
    if matched_aliases.is_empty() {
        format!("matched family '{}'", family)
    } else {
        format!(
            "matched family '{}' and verb aliases {}",
            family,
            matched_aliases.join(", ")
        )
    }
}
