use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::profile::ProfileService;
use crate::services::validation::Validation;
use crate::services::vault_client::VaultClient;
use crate::utils::feature_flags::is_allow_secret_export_enabled;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

const VAULT_PROFILE_TYPE: &str = "vault";
const VAULT_ACTIONS: &[&str] = &[
    "profile_upsert",
    "profile_get",
    "profile_list",
    "profile_delete",
    "profile_test",
];

#[derive(Clone)]
pub struct VaultManager {
    logger: Logger,
    validation: Validation,
    profile_service: Arc<ProfileService>,
    vault_client: Option<Arc<VaultClient>>,
}

impl VaultManager {
    pub fn new(
        logger: Logger,
        validation: Validation,
        profile_service: Arc<ProfileService>,
        vault_client: Option<Arc<VaultClient>>,
    ) -> Self {
        Self {
            logger: logger.child("vault"),
            validation,
            profile_service,
            vault_client,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "profile_upsert" => self.profile_upsert(&args).await,
            "profile_get" => self.profile_get(&args),
            "profile_list" => {
                let profiles = self
                    .profile_service
                    .list_profiles(Some(VAULT_PROFILE_TYPE))?;
                Ok(serde_json::json!({"success": true, "profiles": profiles}))
            }
            "profile_delete" => {
                let name = self.validation.ensure_string(
                    args.get("profile_name").unwrap_or(&Value::Null),
                    "profile_name",
                    true,
                )?;
                self.profile_service.delete_profile(&name)
            }
            "profile_test" => self.profile_test(&args).await,
            _ => Err(unknown_action_error("vault", action, VAULT_ACTIONS)),
        }
    }

    async fn profile_upsert(&self, args: &Value) -> Result<Value, ToolError> {
        let name = self.validation.ensure_string(
            args.get("profile_name").unwrap_or(&Value::Null),
            "profile_name",
            true,
        )?;
        let addr = self.validation.ensure_string(
            args.get("addr")
                .or_else(|| args.get("data").and_then(|v| v.get("addr")))
                .unwrap_or(&Value::Null),
            "addr",
            true,
        )?;

        let namespace = args
            .get("namespace")
            .or_else(|| args.get("data").and_then(|v| v.get("namespace")))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let auth_type = args
            .get("auth_type")
            .or_else(|| args.get("data").and_then(|v| v.get("auth_type")))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());

        let token = args
            .get("token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let role_id = args
            .get("role_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let secret_id = args
            .get("secret_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let token_value = token
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let role_value = role_id
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let secret_value = secret_id
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let inferred_auth = auth_type.clone().unwrap_or_else(|| {
            if token_value.is_some() {
                "token".to_string()
            } else if role_value.is_some() || secret_value.is_some() {
                "approle".to_string()
            } else {
                "none".to_string()
            }
        });

        if inferred_auth != "token" && inferred_auth != "approle" && inferred_auth != "none" {
            return Err(ToolError::invalid_params(format!(
                "Unknown vault auth_type: {}",
                inferred_auth
            ))
            .with_hint("Supported: token, approle, none."));
        }

        if inferred_auth == "token" && token_value.is_none() {
            return Err(ToolError::invalid_params(
                "token is required for vault auth_type=token",
            ));
        }
        if inferred_auth == "approle" {
            if role_value.is_none() {
                return Err(ToolError::invalid_params(
                    "role_id is required for vault auth_type=approle",
                ));
            }
            if secret_value.is_none() {
                return Err(ToolError::invalid_params(
                    "secret_id is required for vault auth_type=approle",
                ));
            }
        }

        let previous = self
            .profile_service
            .get_profile(&name, Some(VAULT_PROFILE_TYPE))
            .ok();

        let data = serde_json::json!({
            "addr": addr,
            "namespace": namespace,
            "auth_type": if inferred_auth == "none" { Value::Null } else { Value::String(inferred_auth.clone()) },
        });
        let secrets = serde_json::json!({
            "token": token,
            "role_id": role_id,
            "secret_id": secret_id,
        });

        let profile_payload = serde_json::json!({
            "type": VAULT_PROFILE_TYPE,
            "data": data,
            "secrets": secrets,
        });
        let _ = self.profile_service.set_profile(&name, &profile_payload)?;

        if let Some(client) = &self.vault_client {
            let check = client.sys_health(&name, Some(args)).await;
            if check.is_err() {
                if let Some(prev) = previous.as_ref() {
                    let rollback = serde_json::json!({
                        "type": VAULT_PROFILE_TYPE,
                        "data": prev.get("data").cloned().unwrap_or(Value::Object(Default::default())),
                        "secrets": prev.get("secrets").cloned().unwrap_or(Value::Object(Default::default())),
                    });
                    let _ = self.profile_service.set_profile(&name, &rollback);
                } else {
                    let _ = self.profile_service.delete_profile(&name);
                }
                return Err(check.err().unwrap());
            }
            if token_value.is_some() {
                let lookup = client.token_lookup_self(&name, Some(args)).await;
                if lookup.is_err() {
                    if let Some(prev) = previous.as_ref() {
                        let rollback = serde_json::json!({
                            "type": VAULT_PROFILE_TYPE,
                            "data": prev.get("data").cloned().unwrap_or(Value::Object(Default::default())),
                            "secrets": prev.get("secrets").cloned().unwrap_or(Value::Object(Default::default())),
                        });
                        let _ = self.profile_service.set_profile(&name, &rollback);
                    } else {
                        let _ = self.profile_service.delete_profile(&name);
                    }
                    return Err(lookup.err().unwrap());
                }
            }
        }

        Ok(serde_json::json!({
            "success": true,
            "profile": {
                "name": name,
                "type": VAULT_PROFILE_TYPE,
                "data": {
                    "addr": data.get("addr").cloned().unwrap_or(Value::Null),
                    "namespace": data.get("namespace").cloned().unwrap_or(Value::Null),
                    "auth_type": if inferred_auth == "none" { Value::Null } else { Value::String(inferred_auth.clone()) },
                },
                "auth": if inferred_auth == "approle" { "approle" } else if token_value.is_some() { "token" } else { "none" },
            }
        }))
    }

    fn profile_get(&self, args: &Value) -> Result<Value, ToolError> {
        let name = self.validation.ensure_string(
            args.get("profile_name").unwrap_or(&Value::Null),
            "profile_name",
            true,
        )?;
        let profile = self
            .profile_service
            .get_profile(&name, Some(VAULT_PROFILE_TYPE))?;
        let include_secrets = args
            .get("include_secrets")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let allow = is_allow_secret_export_enabled();
        if include_secrets && allow {
            return Ok(serde_json::json!({"success": true, "profile": profile}));
        }
        let secret_keys = profile
            .get("secrets")
            .and_then(|v| v.as_object())
            .map(|map| map.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        Ok(serde_json::json!({
            "success": true,
            "profile": {
                "name": profile.get("name").cloned().unwrap_or(Value::String(name)),
                "type": profile.get("type").cloned().unwrap_or(Value::Null),
                "data": profile.get("data").cloned().unwrap_or(Value::Object(Default::default())),
                "secrets": secret_keys,
                "secrets_redacted": true,
            }
        }))
    }

    async fn profile_test(&self, args: &Value) -> Result<Value, ToolError> {
        let name = self.validation.ensure_string(
            args.get("profile_name").unwrap_or(&Value::Null),
            "profile_name",
            true,
        )?;
        let client = self
            .vault_client
            .as_ref()
            .ok_or_else(|| ToolError::internal("Vault client not available"))?;
        let health = client.sys_health(&name, Some(args)).await?;
        let token = match client.token_lookup_self(&name, Some(args)).await {
            Ok(value) => value,
            Err(err) => serde_json::json!({"success": false, "error": err.message}),
        };
        Ok(
            serde_json::json!({"success": true, "profile_name": name, "health": health, "token": token}),
        )
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for VaultManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
