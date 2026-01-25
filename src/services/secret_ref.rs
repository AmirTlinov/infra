use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::profile::ProfileService;
use crate::services::project_resolver::ProjectResolver;
use crate::services::validation::Validation;
use crate::services::vault_client::VaultClient;
use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct SecretRefResolver {
    logger: Logger,
    validation: Validation,
    profile_service: Option<Arc<ProfileService>>,
    vault_client: Option<Arc<VaultClient>>,
    project_resolver: Option<Arc<ProjectResolver>>,
}

impl SecretRefResolver {
    pub fn new(
        logger: Logger,
        validation: Validation,
        profile_service: Option<Arc<ProfileService>>,
        vault_client: Option<Arc<VaultClient>>,
        project_resolver: Option<Arc<ProjectResolver>>,
    ) -> Self {
        Self {
            logger: logger.child("secrets"),
            validation,
            profile_service,
            vault_client,
            project_resolver,
        }
    }

    async fn resolve_vault_profile_name(&self, args: &Value) -> Result<String, ToolError> {
        if let Some(name) = args.get("vault_profile_name").and_then(|v| v.as_str()) {
            return self
                .validation
                .ensure_identifier(name, "vault_profile_name");
        }
        if let Some(name) = args.get("vault_profile").and_then(|v| v.as_str()) {
            return self.validation.ensure_identifier(name, "vault_profile");
        }
        if let Some(resolver) = &self.project_resolver {
            if let Ok(ctx) = resolver.resolve_context(args).await {
                if let Some(target) = ctx.as_ref().and_then(|c| c.get("target")) {
                    if let Some(name) = target.get("vault_profile").and_then(|v| v.as_str()) {
                        return self.validation.ensure_identifier(name, "vault_profile");
                    }
                }
            }
        }
        let service = self.profile_service.as_ref().ok_or_else(|| {
            ToolError::internal("vault profile is required (profileService missing)")
        })?;
        let list = service.list_profiles(Some("vault"))?;
        let profiles = list.as_array().cloned().unwrap_or_default();
        if profiles.len() == 1 {
            if let Some(name) = profiles[0].get("name").and_then(|v| v.as_str()) {
                return Ok(name.to_string());
            }
        }
        if profiles.is_empty() {
            return Err(ToolError::invalid_params(
                "vault profile is required (no vault profiles exist)",
            )
            .with_hint(
                "Create a vault profile first, or pass args.vault_profile_name explicitly."
                    .to_string(),
            ));
        }
        Err(ToolError::invalid_params("vault profile is required when multiple vault profiles exist")
            .with_hint("Pass args.vault_profile_name explicitly (or configure target.vault_profile in project).".to_string()))
    }

    async fn resolve_ref_string(
        &self,
        value: &str,
        args: &Value,
        cache: &mut HashMap<String, String>,
    ) -> Result<String, ToolError> {
        if let Some(existing) = cache.get(value) {
            return Ok(existing.clone());
        }
        let spec = value.trim_start_matches("ref:");
        if spec.starts_with("vault:kv2:") {
            let client = self.vault_client.as_ref().ok_or_else(|| {
                ToolError::internal("vault refs require VaultClient (server misconfiguration)")
                    .with_hint("Enable VaultClient in server bootstrap.")
            })?;
            let reference = spec.trim_start_matches("vault:kv2:");
            let profile_name = self.resolve_vault_profile_name(args).await?;
            let resolved = client.kv2_get(&profile_name, reference, Some(args)).await?;
            cache.insert(value.to_string(), resolved.clone());
            return Ok(resolved);
        }
        if spec.starts_with("env:") {
            let key = spec.trim_start_matches("env:").trim();
            if key.is_empty() {
                return Err(
                    ToolError::invalid_params("ref:env requires a non-empty env var name")
                        .with_hint("Example: \"ref:env:MY_TOKEN\"."),
                );
            }
            let val = std::env::var(key).map_err(|_| {
                ToolError::not_found(format!("ref:env var is not set: {}", key)).with_hint(
                    "Set the env var in the server environment, or use ref:vault:kv2:<mount>/<path>#<key>.".to_string(),
                )
            })?;
            cache.insert(value.to_string(), val.clone());
            return Ok(val);
        }
        let scheme = spec.split(':').next().unwrap_or("unknown");
        Err(
            ToolError::invalid_params(format!("Unknown secret ref scheme: {}", scheme)).with_hint(
                "Supported schemes: ref:vault:kv2:<mount>/<path>#<key>, ref:env:<ENV_VAR>.",
            ),
        )
    }

    pub async fn resolve_deep(&self, input: &Value, args: &Value) -> Result<Value, ToolError> {
        self.logger.debug("resolve_deep", None);
        let mut cache = HashMap::new();
        self.walk_value(input, args, &mut cache).await
    }

    fn walk_value<'a>(
        &'a self,
        value: &'a Value,
        args: &'a Value,
        cache: &'a mut HashMap<String, String>,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        async move {
            if value.is_null() {
                return Ok(Value::Null);
            }
            if let Some(text) = value.as_str() {
                if text.trim_start().starts_with("ref:") {
                    let resolved = self.resolve_ref_string(text, args, cache).await?;
                    return Ok(Value::String(resolved));
                }
                return Ok(Value::String(text.to_string()));
            }
            if let Some(arr) = value.as_array() {
                let mut out = Vec::with_capacity(arr.len());
                for item in arr {
                    out.push(self.walk_value(item, args, cache).await?);
                }
                return Ok(Value::Array(out));
            }
            if let Some(obj) = value.as_object() {
                let mut out = serde_json::Map::new();
                for (key, val) in obj {
                    out.insert(key.clone(), self.walk_value(val, args, cache).await?);
                }
                return Ok(Value::Object(out));
            }
            Ok(value.clone())
        }
        .boxed()
    }
}
