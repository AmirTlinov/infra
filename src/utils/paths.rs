use std::env;
use std::path::{Path, PathBuf};

fn normalize_env_path(value: Option<String>) -> Option<PathBuf> {
    let raw = value?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lowered = trimmed.to_lowercase();
    if lowered == "undefined" || lowered == "null" {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

fn resolve_home_dir() -> Option<PathBuf> {
    env::var("HOME").ok().map(PathBuf::from)
}

fn resolve_xdg_state_dir() -> Option<PathBuf> {
    if let Some(path) = normalize_env_path(env::var("XDG_STATE_HOME").ok()) {
        return Some(path);
    }
    resolve_home_dir().map(|home| home.join(".local").join("state"))
}

fn resolve_entry_dir() -> Option<PathBuf> {
    env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
}

pub fn resolve_profile_base_dir() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_PROFILES_DIR").ok()) {
        return path;
    }
    if let Some(path) = resolve_xdg_state_dir() {
        return path.join("infra");
    }
    resolve_entry_dir().unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

pub fn resolve_profile_key_path() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_PROFILE_KEY_PATH").ok()) {
        return path;
    }
    resolve_profile_base_dir().join(".mcp_profiles.key")
}

pub fn resolve_store_mode() -> &'static str {
    if normalize_env_path(env::var("MCP_PROFILES_DIR").ok()).is_some() {
        return "custom";
    }
    if resolve_xdg_state_dir().is_some() {
        return "xdg";
    }
    "fallback"
}

pub fn resolve_store_info() -> serde_json::Value {
    serde_json::json!({
        "base_dir": resolve_profile_base_dir(),
        "entry_dir": resolve_entry_dir(),
        "mode": resolve_store_mode(),
    })
}

pub fn resolve_profiles_path() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_PROFILES_PATH").ok()) {
        return path;
    }
    resolve_profile_base_dir().join("profiles.json")
}

pub fn resolve_state_path() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_STATE_PATH").ok()) {
        return path;
    }
    resolve_profile_base_dir().join("state.json")
}

pub fn resolve_projects_path() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_PROJECTS_PATH").ok()) {
        return path;
    }
    resolve_profile_base_dir().join("projects.json")
}

pub fn resolve_runbooks_path() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_RUNBOOKS_PATH").ok()) {
        return path;
    }
    resolve_profile_base_dir().join("runbooks.json")
}

pub fn resolve_default_runbooks_path() -> Option<PathBuf> {
    if let Some(path) = normalize_env_path(env::var("MCP_DEFAULT_RUNBOOKS_PATH").ok()) {
        return Some(path);
    }
    let entry_dir = resolve_entry_dir();
    let mut candidates = Vec::new();
    if let Some(entry) = entry_dir {
        candidates.push(entry.join("runbooks.json"));
        candidates.push(entry.join("..").join("runbooks.json"));
    }
    candidates.push(PathBuf::from("runbooks.json"));
    for candidate in &candidates {
        if candidate.exists() {
            return Some(candidate.clone());
        }
    }
    candidates.into_iter().next()
}

pub fn resolve_capabilities_path() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_CAPABILITIES_PATH").ok()) {
        return path;
    }
    resolve_profile_base_dir().join("capabilities.json")
}

pub fn resolve_default_capabilities_path() -> Option<PathBuf> {
    if let Some(path) = normalize_env_path(env::var("MCP_DEFAULT_CAPABILITIES_PATH").ok()) {
        return Some(path);
    }
    let entry_dir = resolve_entry_dir();
    let mut candidates = Vec::new();
    if let Some(entry) = entry_dir {
        candidates.push(entry.join("capabilities.json"));
        candidates.push(entry.join("..").join("capabilities.json"));
    }
    candidates.push(PathBuf::from("capabilities.json"));
    for candidate in &candidates {
        if candidate.exists() {
            return Some(candidate.clone());
        }
    }
    candidates.into_iter().next()
}

pub fn resolve_context_path() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_CONTEXT_PATH").ok()) {
        return path;
    }
    resolve_profile_base_dir().join("context.json")
}

pub fn resolve_evidence_dir() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_EVIDENCE_DIR").ok()) {
        return path;
    }
    resolve_profile_base_dir().join(".infra").join("evidence")
}

pub fn resolve_aliases_path() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_ALIASES_PATH").ok()) {
        return path;
    }
    resolve_profile_base_dir().join("aliases.json")
}

pub fn resolve_presets_path() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_PRESETS_PATH").ok()) {
        return path;
    }
    resolve_profile_base_dir().join("presets.json")
}

pub fn resolve_audit_path() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_AUDIT_PATH").ok()) {
        return path;
    }
    resolve_profile_base_dir().join("audit.jsonl")
}

pub fn resolve_jobs_path() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_JOBS_PATH").ok()) {
        return path;
    }
    if let Some(path) = normalize_env_path(env::var("INFRA_JOBS_PATH").ok()) {
        return path;
    }
    resolve_profile_base_dir().join("jobs.json")
}

pub fn resolve_cache_dir() -> PathBuf {
    if let Some(path) = normalize_env_path(env::var("MCP_CACHE_DIR").ok()) {
        return path;
    }
    resolve_profile_base_dir().join("cache")
}

pub fn resolve_context_repo_root() -> Option<PathBuf> {
    normalize_env_path(env::var("INFRA_CONTEXT_REPO_ROOT").ok())
        .or_else(|| normalize_env_path(env::var("MCP_CONTEXT_REPO_ROOT").ok()))
}

pub fn ensure_dir_exists(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}
