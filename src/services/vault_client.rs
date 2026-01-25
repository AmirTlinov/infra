use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::profile::ProfileService;
use crate::services::validation::Validation;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT};
use reqwest::{Client, Method};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

#[derive(Clone)]
struct VaultProfile {
    addr: String,
    namespace: Option<String>,
    token: Option<String>,
    role_id: Option<String>,
    secret_id: Option<String>,
}

#[derive(Clone)]
pub struct VaultClient {
    logger: Logger,
    validation: Validation,
    profile_service: Arc<ProfileService>,
    client: Client,
    default_timeout_ms: u64,
    default_retries: u32,
}

impl VaultClient {
    pub fn new(
        logger: Logger,
        validation: Validation,
        profile_service: Arc<ProfileService>,
    ) -> Self {
        let client = Client::builder()
            .user_agent("infra/7.0")
            .build()
            .expect("reqwest client");
        Self {
            logger: logger.child("vault"),
            validation,
            profile_service,
            client,
            default_timeout_ms: 15_000,
            default_retries: 1,
        }
    }

    async fn load_profile(&self, profile_name: &str) -> Result<VaultProfile, ToolError> {
        let profile_name = self
            .validation
            .ensure_identifier(profile_name, "profile_name")?;
        let profile = self
            .profile_service
            .get_profile(&profile_name, Some("vault"))?;
        let data = profile
            .get("data")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let secrets = profile
            .get("secrets")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        let addr = normalize_base_url(data.get("addr").and_then(|v| v.as_str()))?;
        let namespace = data
            .get("namespace")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let token = secrets
            .get("token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let role_id = secrets
            .get("role_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let secret_id = secrets
            .get("secret_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(VaultProfile {
            addr,
            namespace,
            token,
            role_id,
            secret_id,
        })
    }

    fn build_headers(&self, token: Option<&str>, namespace: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        if let Some(ns) = namespace {
            if let Ok(value) = HeaderValue::from_str(ns) {
                headers.insert("X-Vault-Namespace", value);
            }
        }
        if let Some(token) = token {
            if let Ok(value) = HeaderValue::from_str(token) {
                headers.insert("X-Vault-Token", value);
            }
        }
        headers
    }

    async fn request_json(
        &self,
        url: &str,
        method: Method,
        headers: HeaderMap,
        body: Option<Value>,
        timeout_ms: Option<u64>,
        retries: Option<u32>,
    ) -> Result<Value, ToolError> {
        self.logger.debug("request_json", None);
        let timeout_ms = timeout_ms.unwrap_or(self.default_timeout_ms);
        let max_retries = retries.unwrap_or(self.default_retries);
        let mut attempt = 0u32;

        loop {
            attempt += 1;
            let mut request = self
                .client
                .request(method.clone(), url)
                .headers(headers.clone());
            if let Some(body) = &body {
                request = request.json(body);
            }

            let response = tokio::time::timeout(Duration::from_millis(timeout_ms), request.send())
                .await
                .map_err(|_| ToolError::timeout("Vault request timed out"))?
                .map_err(|err| ToolError::retryable(format!("Vault request failed: {}", err)))?;

            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            let parsed: Option<Value> = serde_json::from_str(&text).ok();

            if !status.is_success() {
                let details = parsed.as_ref().and_then(parse_vault_error);
                let message = if let Some(details) = details {
                    format!("Vault request failed ({}): {}", status.as_u16(), details)
                } else {
                    format!("Vault request failed ({})", status.as_u16())
                };
                let err = if status.as_u16() == 401 || status.as_u16() == 403 {
                    ToolError::denied(message)
                } else if status.as_u16() == 404 {
                    ToolError::not_found(message)
                } else if status.as_u16() == 429 || status.is_server_error() {
                    ToolError::retryable(message)
                        .with_hint("Retry later or increase timeout/retries.")
                } else {
                    ToolError::invalid_params(message)
                };

                if (err.kind == crate::errors::ToolErrorKind::Retryable || err.retryable)
                    && attempt <= max_retries
                {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    continue;
                }
                return Err(err);
            }

            return Ok(parsed.unwrap_or(Value::Null));
        }
    }

    fn can_approle(profile: &VaultProfile) -> bool {
        profile
            .role_id
            .as_ref()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
            && profile
                .secret_id
                .as_ref()
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
    }

    async fn login_approle(
        &self,
        profile: &VaultProfile,
        options: Option<&Value>,
    ) -> Result<String, ToolError> {
        let role_id = profile
            .role_id
            .as_ref()
            .ok_or_else(|| ToolError::invalid_params("role_id is required for approle login"))?;
        let secret_id = profile
            .secret_id
            .as_ref()
            .ok_or_else(|| ToolError::invalid_params("secret_id is required for approle login"))?;

        let timeout_ms = options
            .and_then(|v| v.get("timeout_ms"))
            .and_then(|v| v.as_u64());
        let retries = options
            .and_then(|v| v.get("retries"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        let url = format!("{}/v1/auth/approle/login", profile.addr);
        let body = serde_json::json!({ "role_id": role_id, "secret_id": secret_id });
        let response = self
            .request_json(
                &url,
                Method::POST,
                self.build_headers(None, profile.namespace.as_deref()),
                Some(body),
                timeout_ms,
                retries,
            )
            .await?;
        let token = response
            .get("auth")
            .and_then(|v| v.get("client_token"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if token.trim().is_empty() {
            return Err(ToolError::internal(
                "Vault approle login did not return client_token",
            ));
        }
        Ok(token)
    }

    async fn ensure_token(
        &self,
        profile: &VaultProfile,
        options: Option<&Value>,
    ) -> Result<String, ToolError> {
        if let Some(token) = profile.token.as_ref() {
            if !token.trim().is_empty() {
                return Ok(token.to_string());
            }
        }
        if !Self::can_approle(profile) {
            return Err(
                ToolError::invalid_params("Vault token is required for this operation").with_hint(
                    "Set profile.secrets.token, or configure AppRole (role_id + secret_id).",
                ),
            );
        }
        self.login_approle(profile, options).await
    }

    pub async fn sys_health(
        &self,
        profile_name: &str,
        options: Option<&Value>,
    ) -> Result<Value, ToolError> {
        let profile = self.load_profile(profile_name).await?;
        let timeout_ms = options
            .and_then(|v| v.get("timeout_ms"))
            .and_then(|v| v.as_u64());
        let retries = options
            .and_then(|v| v.get("retries"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        let url = format!("{}/v1/sys/health", profile.addr);
        self.request_json(
            &url,
            Method::GET,
            self.build_headers(profile.token.as_deref(), profile.namespace.as_deref()),
            None,
            timeout_ms,
            retries,
        )
        .await
    }

    pub async fn token_lookup_self(
        &self,
        profile_name: &str,
        options: Option<&Value>,
    ) -> Result<Value, ToolError> {
        let profile = self.load_profile(profile_name).await?;
        let timeout_ms = options
            .and_then(|v| v.get("timeout_ms"))
            .and_then(|v| v.as_u64());
        let retries = options
            .and_then(|v| v.get("retries"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        let token = self.ensure_token(&profile, options).await?;
        let url = format!("{}/v1/auth/token/lookup-self", profile.addr);
        let response = self
            .request_json(
                &url,
                Method::GET,
                self.build_headers(Some(&token), profile.namespace.as_deref()),
                None,
                timeout_ms,
                retries,
            )
            .await;
        match response {
            Ok(value) => Ok(value),
            Err(err) => {
                if err.kind == crate::errors::ToolErrorKind::Denied && Self::can_approle(&profile) {
                    let fresh = self.login_approle(&profile, options).await?;
                    self.request_json(
                        &url,
                        Method::GET,
                        self.build_headers(Some(&fresh), profile.namespace.as_deref()),
                        None,
                        timeout_ms,
                        retries,
                    )
                    .await
                } else {
                    Err(err)
                }
            }
        }
    }

    pub async fn kv2_get(
        &self,
        profile_name: &str,
        reference: &str,
        options: Option<&Value>,
    ) -> Result<String, ToolError> {
        let profile = self.load_profile(profile_name).await?;
        let ref_value = reference.trim();
        let (path_part, key) = ref_value.split_once('#').ok_or_else(|| {
            ToolError::invalid_params("Vault kv2 ref must include #key (e.g. secret/app#TOKEN)")
        })?;
        let path_part = path_part.trim();
        let key = key.trim();
        if path_part.is_empty() || key.is_empty() {
            return Err(ToolError::invalid_params(
                "Vault kv2 ref must include mount/path and key",
            ));
        }
        let mut pieces = path_part.splitn(2, '/');
        let mount = pieces.next().unwrap_or("").trim();
        let path = pieces.next().unwrap_or("").trim();
        if mount.is_empty() || path.is_empty() {
            return Err(ToolError::invalid_params(
                "Vault kv2 ref must be <mount>/<path>#<key>",
            ));
        }

        let timeout_ms = options
            .and_then(|v| v.get("timeout_ms"))
            .and_then(|v| v.as_u64());
        let retries = options
            .and_then(|v| v.get("retries"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        let token = self.ensure_token(&profile, options).await?;
        let url = format!("{}/v1/{}/data/{}", profile.addr, mount, path);
        let response = self
            .request_json(
                &url,
                Method::GET,
                self.build_headers(Some(&token), profile.namespace.as_deref()),
                None,
                timeout_ms,
                retries,
            )
            .await?;
        let value = response
            .get("data")
            .and_then(|v| v.get("data"))
            .and_then(|v| v.get(key))
            .ok_or_else(|| ToolError::not_found(format!("Vault kv2 key '{}' not found", key)))?;
        if value.is_null() {
            return Err(ToolError::not_found(format!(
                "Vault kv2 key '{}' not found",
                key
            )));
        }
        Ok(value.as_str().unwrap_or(&value.to_string()).to_string())
    }
}

fn normalize_base_url(raw: Option<&str>) -> Result<String, ToolError> {
    let raw = raw.unwrap_or("").trim();
    if raw.is_empty() {
        return Err(ToolError::invalid_params("vault addr is required")
            .with_hint("Set profile.data.addr, e.g. \"https://vault.example.com\"."));
    }
    let mut url = Url::parse(raw).map_err(|_| {
        ToolError::invalid_params("Invalid vault addr URL")
            .with_hint("Expected a valid URL, e.g. \"https://vault.example.com\".")
            .with_details(serde_json::json!({ "addr": raw }))
    })?;
    url.set_fragment(None);
    url.set_query(None);
    let normalized = format!("{}{}", url.origin().ascii_serialization(), url.path());
    Ok(normalized.trim_end_matches('/').to_string())
}

fn parse_vault_error(value: &Value) -> Option<String> {
    let errors = value.get("errors").and_then(|v| v.as_array())?;
    if errors.is_empty() {
        return None;
    }
    let joined = errors
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect::<Vec<_>>()
        .join("; ");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}
