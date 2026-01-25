use crate::errors::ToolError;
use crate::services::context::ContextService;
use crate::services::logger::Logger;
use crate::services::profile::ProfileService;
use crate::services::project_resolver::ProjectResolver;
use crate::utils::fs_atomic::path_exists;
use crate::utils::user_paths::expand_home_path;
use serde_json::Value;
use std::sync::Arc;

const PROFILE_TYPES: &[(&str, &str)] = &[
    ("ssh_profile", "ssh"),
    ("env_profile", "env"),
    ("postgres_profile", "postgresql"),
    ("api_profile", "api"),
    ("vault_profile", "vault"),
];

#[derive(Clone)]
pub struct ContextSessionService {
    logger: Logger,
    context_service: Arc<ContextService>,
    project_resolver: Option<Arc<ProjectResolver>>,
    profile_service: Option<Arc<ProfileService>>,
}

impl ContextSessionService {
    pub fn new(
        logger: Logger,
        context_service: Arc<ContextService>,
        project_resolver: Option<Arc<ProjectResolver>>,
        profile_service: Option<Arc<ProfileService>>,
    ) -> Self {
        Self {
            logger: logger.child("context_session"),
            context_service,
            project_resolver,
            profile_service,
        }
    }

    pub async fn resolve(&self, args: &Value) -> Result<Value, ToolError> {
        self.logger.debug("resolve", None);
        let mut errors: Vec<Value> = Vec::new();
        let mut warnings: Vec<Value> = Vec::new();
        let mut hints: Vec<Value> = Vec::new();

        let context = match self.context_service.get_context(args).await {
            Ok(result) => result
                .get("context")
                .cloned()
                .unwrap_or(Value::Object(Default::default())),
            Err(err) => {
                errors.push(serde_json::json!({
                    "code": "context_failed",
                    "message": err.message,
                }));
                Value::Object(Default::default())
            }
        };

        let mut project_context: Option<Value> = None;
        if let Some(resolver) = &self.project_resolver {
            project_context = match resolver.resolve_context(args).await {
                Ok(ctx) => ctx,
                Err(err) => Some(serde_json::json!({ "error": err.message })),
            };
        }
        if let Some(err) = project_context.as_ref().and_then(|v| v.get("error")) {
            errors.push(serde_json::json!({
                "code": "project_resolution_failed",
                "message": err.as_str().unwrap_or("project resolution failed"),
            }));
        }

        let target = project_context
            .as_ref()
            .and_then(|v| v.get("target"))
            .and_then(|v| v.as_object());

        let mut bindings_profiles = serde_json::Map::new();
        let mut bindings_paths = serde_json::Map::new();
        let mut bindings_urls = serde_json::Map::new();

        if let Some(target) = target {
            for (key, _) in PROFILE_TYPES.iter() {
                if let Some(value) = normalize_string(target.get(*key)) {
                    bindings_profiles.insert(key.to_string(), Value::String(value));
                }
            }

            if let Some(value) = normalize_string(target.get("kubeconfig")) {
                bindings_paths.insert("kubeconfig".to_string(), Value::String(value));
            }
            if let Some(value) = normalize_string(target.get("sops_age_key_file")) {
                bindings_paths.insert("sops_age_key_file".to_string(), Value::String(value));
            }
            if let Some(value) =
                normalize_string(target.get("repo_path").or_else(|| target.get("repo_root")))
            {
                bindings_paths.insert("repo_root".to_string(), Value::String(value));
            }
            if let Some(value) = normalize_string(target.get("cwd")) {
                bindings_paths.insert("cwd".to_string(), Value::String(value));
            }
            if let Some(value) = normalize_string(target.get("api_base_url")) {
                bindings_urls.insert("api_base_url".to_string(), Value::String(value));
            }
            if let Some(value) = normalize_string(target.get("registry_url")) {
                bindings_urls.insert("registry_url".to_string(), Value::String(value));
            }
        }

        let mut effective_tags = context
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();
        if bindings_paths.get("kubeconfig").is_some() {
            effective_tags.push("k8s".to_string());
        }
        if bindings_profiles.get("ssh_profile").is_some() {
            effective_tags.push("ssh".to_string());
        }
        if bindings_urls.get("api_base_url").is_some() {
            effective_tags.push("api".to_string());
        }
        if bindings_urls.get("registry_url").is_some() {
            effective_tags.push("registry".to_string());
        }
        effective_tags.sort();
        effective_tags.dedup();

        let mut effective_context = context.clone();
        if let Value::Object(map) = &mut effective_context {
            map.insert(
                "tags".to_string(),
                Value::Array(effective_tags.into_iter().map(Value::String).collect()),
            );
        }

        let bindings = serde_json::json!({
            "profiles": Value::Object(bindings_profiles),
            "paths": Value::Object(bindings_paths),
            "urls": Value::Object(bindings_urls),
        });

        self.check_bindings(&bindings, &mut warnings, &mut hints)?;

        if let Some(profile_service) = self.profile_service.as_ref() {
            self.check_profiles(profile_service, &bindings, &mut warnings)?;
        }

        if let Some(api_base_url) = bindings
            .get("urls")
            .and_then(|v| v.get("api_base_url"))
            .and_then(|v| v.as_str())
        {
            if let Some(var) = read_ref_env(api_base_url) {
                if std::env::var(&var).is_err() {
                    warnings.push(serde_json::json!({
                        "code": "env_ref_missing",
                        "message": format!("Переменная окружения не задана: {}", var),
                        "meta": { "ref": api_base_url },
                    }));
                }
            }
        }

        let include_project = project_context
            .as_ref()
            .filter(|v| v.get("error").is_none())
            .cloned()
            .unwrap_or(Value::Null);

        Ok(serde_json::json!({
            "context": context,
            "effective_context": effective_context,
            "project_context": include_project,
            "diagnostics": {
                "errors": errors,
                "warnings": warnings,
                "hints": hints,
            },
            "bindings": bindings,
        }))
    }

    fn check_bindings(
        &self,
        bindings: &Value,
        warnings: &mut Vec<Value>,
        hints: &mut Vec<Value>,
    ) -> Result<(), ToolError> {
        let paths = bindings.get("paths").and_then(|v| v.as_object());
        let Some(paths) = paths else {
            return Ok(());
        };
        for (key, label) in [
            ("kubeconfig", "kubeconfig"),
            ("sops_age_key_file", "sops_age_key_file"),
            ("repo_root", "repo_root"),
        ] {
            let Some(raw) = paths.get(key).and_then(|v| v.as_str()) else {
                continue;
            };
            if let Some(var) = read_ref_env(raw) {
                if std::env::var(&var).is_err() {
                    warnings.push(serde_json::json!({
                        "code": "env_ref_missing",
                        "message": format!("Переменная окружения не задана: {}", var),
                        "meta": { "ref": raw },
                    }));
                }
                continue;
            }
            if let Some(ref_vault) = read_ref_vault(raw) {
                hints.push(serde_json::json!({
                    "code": "vault_ref_detected",
                    "message": "Обнаружена vault-ссылка в путях. Убедитесь, что настроен vault_profile.",
                    "meta": { "ref": ref_vault },
                }));
                continue;
            }
            let expanded = expand_home_path(raw);
            if !path_exists(expanded) {
                warnings.push(serde_json::json!({
                    "code": "path_missing",
                    "message": format!("Файл не найден: {}", raw),
                    "meta": { "key": label },
                }));
            }
        }
        Ok(())
    }

    fn check_profiles(
        &self,
        profile_service: &ProfileService,
        bindings: &Value,
        warnings: &mut Vec<Value>,
    ) -> Result<(), ToolError> {
        let Some(profiles) = bindings.get("profiles").and_then(|v| v.as_object()) else {
            return Ok(());
        };
        for (key, profile) in profiles.iter() {
            let Some(profile_name) = profile.as_str() else {
                continue;
            };
            if profile_name.trim().is_empty() {
                continue;
            }
            if !profile_service.has_profile(profile_name) {
                warnings.push(serde_json::json!({
                    "code": "missing_profile",
                    "message": format!("Профиль не найден: {}", profile_name),
                    "meta": { "key": key },
                }));
                continue;
            }
        }
        Ok(())
    }
}

fn normalize_string(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn read_ref_env(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let prefix = "ref:env:";
    if !trimmed.starts_with(prefix) {
        return None;
    }
    let key = trimmed.trim_start_matches(prefix).trim();
    if key.is_empty() {
        None
    } else {
        Some(key.to_string())
    }
}

fn read_ref_vault(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.starts_with("ref:vault:") {
        Some(trimmed.to_string())
    } else {
        None
    }
}
