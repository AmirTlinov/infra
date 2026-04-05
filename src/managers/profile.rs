use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::profile::ProfileService;
use crate::services::validation::Validation;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

pub(crate) const PROFILE_ACTIONS: &[&str] = &["list", "get", "set", "delete"];

#[derive(Clone)]
pub struct ProfileManager {
    logger: Logger,
    profile_service: Arc<ProfileService>,
}

impl ProfileManager {
    pub fn new(logger: Logger, profile_service: Arc<ProfileService>) -> Self {
        Self {
            logger: logger.child("profile"),
            profile_service,
        }
    }

    fn allow_secret_export() -> bool {
        std::env::var("INFRA_ALLOW_SECRET_EXPORT")
            .ok()
            .map(|value| value.trim() == "1" || value.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }

    fn resolve_profile_name(&self, args: &Value) -> Result<String, ToolError> {
        let raw = args
            .get("name")
            .or_else(|| args.get("profile_name"))
            .or_else(|| args.get("profile").and_then(|value| value.get("name")))
            .unwrap_or(&Value::Null);
        let validation = Validation::new();
        let name = validation.ensure_string(raw, "Profile name", true)?;
        validation.ensure_identifier(&name, "Profile name")
    }

    fn redact_profile(&self, fallback_name: &str, profile: &Value) -> Value {
        let secret_keys = profile
            .get("secrets")
            .and_then(|value| value.as_object())
            .map(|map| {
                let mut keys: Vec<String> = map.keys().cloned().collect();
                keys.sort();
                keys
            })
            .unwrap_or_default();

        let mut safe = serde_json::json!({
            "name": profile.get("name").cloned().unwrap_or(Value::String(fallback_name.to_string())),
            "type": profile.get("type").cloned().unwrap_or(Value::Null),
            "data": profile.get("data").cloned().unwrap_or(Value::Object(Default::default())),
        });

        if !secret_keys.is_empty() {
            let values = secret_keys.into_iter().map(Value::String).collect();
            if let Value::Object(map) = &mut safe {
                map.insert("secrets".to_string(), Value::Array(values));
                map.insert("secrets_redacted".to_string(), Value::Bool(true));
            }
        }

        safe
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|value| value.as_str()).unwrap_or("") {
            "list" => {
                let filter_type = args.get("type").and_then(|value| value.as_str());
                let profiles = self.profile_service.list_profiles(filter_type)?;
                Ok(serde_json::json!({
                    "success": true,
                    "profiles": profiles,
                }))
            }
            "get" => {
                let name = self.resolve_profile_name(&args)?;
                let expected_type = args.get("type").and_then(|value| value.as_str());
                let profile = self.profile_service.get_profile(&name, expected_type)?;
                let include_secrets = args
                    .get("include_secrets")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                let profile = if include_secrets && Self::allow_secret_export() {
                    profile
                } else {
                    self.redact_profile(&name, &profile)
                };
                Ok(serde_json::json!({
                    "success": true,
                    "profile": profile,
                }))
            }
            "set" => {
                let name = self.resolve_profile_name(&args)?;
                let mut config = args
                    .get("profile")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Default::default()));
                if let Value::Object(map) = &mut config {
                    for field in ["type", "data", "secrets"] {
                        if let Some(value) = args.get(field) {
                            map.insert(field.to_string(), value.clone());
                        }
                    }
                }
                let updated = self.profile_service.set_profile(&name, &config)?;
                Ok(serde_json::json!({
                    "success": true,
                    "profile": self.redact_profile(&name, &updated),
                }))
            }
            "delete" => {
                let name = self.resolve_profile_name(&args)?;
                self.profile_service.delete_profile(&name)?;
                Ok(serde_json::json!({
                    "success": true,
                    "profile": { "name": name },
                }))
            }
            _ => Err(unknown_action_error("profile", action, PROFILE_ACTIONS)),
        }
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for ProfileManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
