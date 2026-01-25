use crate::constants::{
    cache as cache_constants, network as network_constants, pagination as pagination_constants,
    protocols::ALLOWED_HTTP, retry as retry_constants,
};
use crate::errors::ToolError;
use crate::services::cache::CacheService;
use crate::services::logger::Logger;
use crate::services::profile::ProfileService;
use crate::services::project_resolver::ProjectResolver;
use crate::services::secret_ref::SecretRefResolver;
use crate::services::validation::Validation;
use crate::utils::artifacts::{
    build_tool_call_file_ref, create_artifact_write_stream, resolve_context_root,
};
use crate::utils::data_path::get_path_value;
use crate::utils::redact::redact_text;
use crate::utils::tool_errors::unknown_action_error;
use crate::utils::user_paths::expand_home_path;
use base64::Engine;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Method};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use url::Url;

const API_PROFILE_TYPE: &str = "api";
const API_ACTIONS: &[&str] = &[
    "profile_upsert",
    "profile_get",
    "profile_list",
    "profile_delete",
    "request",
    "paginate",
    "download",
    "check",
    "smoke_http",
];

#[derive(Clone)]
pub struct ApiManager {
    logger: Logger,
    validation: Validation,
    profile_service: Arc<ProfileService>,
    cache_service: Option<Arc<CacheService>>,
    project_resolver: Option<Arc<ProjectResolver>>,
    secret_ref_resolver: Option<Arc<SecretRefResolver>>,
    clients: Arc<Mutex<HashMap<(bool, bool), Client>>>,
    token_cache: Arc<Mutex<HashMap<String, CachedToken>>>,
}

#[derive(Clone)]
struct CachedToken {
    token: String,
    expires_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
enum StreamMode {
    Full,
    Capped,
}

#[derive(Clone, Debug)]
struct CachePolicy {
    enabled: bool,
    ttl_ms: Option<u64>,
    cache_errors: bool,
    key: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct RetryPolicy {
    pub(crate) enabled: bool,
    pub(crate) max_attempts: usize,
    pub(crate) base_delay_ms: u64,
    pub(crate) max_delay_ms: u64,
    pub(crate) jitter: f64,
    pub(crate) status_codes: Vec<u16>,
    pub(crate) methods: Option<Vec<String>>,
    pub(crate) retry_on_network_error: bool,
    pub(crate) respect_retry_after: bool,
}

#[derive(Clone, Debug)]
struct PaginationConfig {
    kind: String,
    param: String,
    size_param: String,
    size: usize,
    start: i64,
    max_pages: usize,
    item_path: Option<String>,
    cursor_path: Option<String>,
    link_rel: String,
    stop_on_empty: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ApiProfile {
    pub(crate) name: Option<String>,
    pub(crate) data: serde_json::Map<String, Value>,
    pub(crate) auth: Option<Value>,
    pub(crate) auth_provider: Option<Value>,
    pub(crate) retry: Option<Value>,
    pub(crate) pagination: Option<Value>,
    pub(crate) cache: Option<Value>,
}

#[derive(Debug)]
struct BodyCapture {
    buffer: Vec<u8>,
    body_read_bytes: u64,
    body_captured_bytes: u64,
    body_truncated: bool,
    body_ref: Option<Value>,
    body_ref_truncated: Option<bool>,
}

impl ApiManager {
    pub fn new(
        logger: Logger,
        validation: Validation,
        profile_service: Arc<ProfileService>,
        cache_service: Option<Arc<CacheService>>,
        project_resolver: Option<Arc<ProjectResolver>>,
        secret_ref_resolver: Option<Arc<SecretRefResolver>>,
    ) -> Self {
        Self {
            logger: logger.child("api"),
            validation,
            profile_service,
            cache_service,
            project_resolver,
            secret_ref_resolver,
            clients: Arc::new(Mutex::new(HashMap::new())),
            token_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        let action_name = action.and_then(|v| v.as_str()).unwrap_or("");
        match action_name {
            "profile_upsert" => self.profile_upsert(&args),
            "profile_get" => self.profile_get(&args),
            "profile_list" => self.profile_list(),
            "profile_delete" => self.profile_delete(&args),
            "request" => self.request(args).await,
            "paginate" => self.paginate(args).await,
            "download" => self.download(args).await,
            "check" => self.check_api(args).await,
            "smoke_http" => self.smoke_http(args).await,
            _ => Err(unknown_action_error("api", action, API_ACTIONS)),
        }
    }

    fn profile_upsert(&self, args: &Value) -> Result<Value, ToolError> {
        let name = self.validation.ensure_string(
            args.get("profile_name").unwrap_or(&Value::Null),
            "profile_name",
            true,
        )?;
        let base = args
            .get("base_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let headers = self.validation.ensure_headers(args.get("headers"))?;
        let (data_auth, secrets) = split_auth(args.get("auth"));
        let (provider_data, provider_secrets) = split_auth_provider(args.get("auth_provider"));

        let mut data = serde_json::Map::new();
        if let Some(base) = base {
            data.insert("base_url".to_string(), Value::String(base));
        }
        if !headers.is_empty() {
            data.insert("headers".to_string(), Value::Object(headers));
        }
        if let Some(auth) = data_auth {
            data.insert("auth".to_string(), auth);
        }
        if let Some(provider) = provider_data {
            data.insert("auth_provider".to_string(), provider);
        }
        if let Some(retry) = args.get("retry") {
            data.insert("retry".to_string(), retry.clone());
        }
        if let Some(pagination) = args.get("pagination") {
            data.insert("pagination".to_string(), pagination.clone());
        }
        if let Some(cache) = args.get("cache") {
            data.insert("cache".to_string(), cache.clone());
        }
        if let Some(timeout) = args.get("timeout_ms") {
            data.insert("timeout_ms".to_string(), timeout.clone());
        }
        if let Some(response_type) = args.get("response_type") {
            data.insert("response_type".to_string(), response_type.clone());
        }
        if let Some(redirect) = args.get("redirect") {
            data.insert("redirect".to_string(), redirect.clone());
        }

        let mut secrets_map = serde_json::Map::new();
        if let Some(obj) = secrets {
            secrets_map.extend(obj);
        }
        if let Some(obj) = provider_secrets {
            secrets_map.extend(obj);
        }

        let mut config = serde_json::Map::new();
        config.insert(
            "type".to_string(),
            Value::String(API_PROFILE_TYPE.to_string()),
        );
        config.insert("data".to_string(), Value::Object(data));
        if !secrets_map.is_empty() {
            config.insert("secrets".to_string(), Value::Object(secrets_map));
        }

        let profile = self
            .profile_service
            .set_profile(&name, &Value::Object(config))?;
        Ok(serde_json::json!({"success": true, "profile": profile}))
    }

    fn profile_get(&self, args: &Value) -> Result<Value, ToolError> {
        let name = self.validation.ensure_string(
            args.get("profile_name").unwrap_or(&Value::Null),
            "profile_name",
            true,
        )?;
        let profile = self
            .profile_service
            .get_profile(&name, Some(API_PROFILE_TYPE))?;
        let include_secrets = args
            .get("include_secrets")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let allow = std::env::var("INFRA_ALLOW_SECRET_EXPORT")
            .ok()
            .filter(|v| v.trim() == "1" || v.trim().eq_ignore_ascii_case("true"))
            .is_some();
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

    fn profile_list(&self) -> Result<Value, ToolError> {
        let profiles = self.profile_service.list_profiles(Some(API_PROFILE_TYPE))?;
        Ok(serde_json::json!({"success": true, "profiles": profiles}))
    }

    fn profile_delete(&self, args: &Value) -> Result<Value, ToolError> {
        let name = self.validation.ensure_string(
            args.get("profile_name").unwrap_or(&Value::Null),
            "profile_name",
            true,
        )?;
        self.profile_service.delete_profile(&name)
    }

    async fn request(&self, args: Value) -> Result<Value, ToolError> {
        let profile = self
            .resolve_profile(args.get("profile_name"), &args)
            .await?;
        let mut auth = profile.auth.clone();
        if args.get("auth").is_some() {
            auth = normalize_auth_value(args.get("auth").unwrap_or(&Value::Null));
        }
        let auth_provider = if args.get("auth_provider").is_some() {
            args.get("auth_provider").cloned()
        } else {
            profile.auth_provider.clone()
        };

        let resolved_provider = self
            .resolve_auth_provider(auth_provider, profile.name.as_deref(), &args)
            .await?;
        if resolved_provider.is_some() {
            auth = resolved_provider;
        }

        let cache_policy = self.normalize_cache_policy(args.get("cache"), profile.cache.as_ref());
        let mut cache_key = None;
        if cache_policy.enabled {
            if let Some(cache_service) = self.cache_service.as_ref() {
                cache_key = cache_policy.key.clone().or_else(|| {
                    let config = self.build_request_config(&args, &profile, auth.as_ref(), None).ok()?;
                    let payload = serde_json::json!({
                        "url": config.url,
                        "method": config.method.as_str(),
                        "headers": config.headers_raw,
                        "body": args.get("body").cloned().or_else(|| args.get("data").cloned()).or_else(|| args.get("form").cloned()).or_else(|| args.get("body_base64").cloned()),
                    });
                    Some(cache_service.build_key(&payload))
                });
                if let Some(key) = cache_key.as_ref() {
                    if let Some(cached) = cache_service.get_json(key, cache_policy.ttl_ms)? {
                        if let Some(value) = cached.get("value").cloned() {
                            let created_at = cached.get("created_at").and_then(|v| v.as_str());
                            let age_ms = created_at
                                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                                .map(|dt| {
                                    chrono::Utc::now().timestamp_millis() - dt.timestamp_millis()
                                });
                            let mut result = value;
                            if let Value::Object(map) = &mut result {
                                map.insert(
                                    "cache".to_string(),
                                    serde_json::json!({
                                        "hit": true,
                                        "key": key,
                                        "created_at": created_at,
                                        "age_ms": age_ms,
                                    }),
                                );
                            }
                            return Ok(result);
                        }
                    }
                }
            }
        }

        let response = self
            .request_with_retry(&args, &profile, auth.as_ref())
            .await?;

        if cache_policy.enabled {
            if let (Some(cache_service), Some(key)) =
                (self.cache_service.as_ref(), cache_key.as_ref())
            {
                if response
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true)
                    || cache_policy.cache_errors
                {
                    let _ = cache_service.set_json(
                        key,
                        &response,
                        cache_policy.ttl_ms,
                        Some(serde_json::json!({
                            "url": response.get("url").cloned().unwrap_or(Value::Null),
                            "method": response.get("method").cloned().unwrap_or(Value::Null),
                        })),
                    );
                }
                if let Some(mut map) = response.as_object().cloned() {
                    map.insert(
                        "cache".to_string(),
                        serde_json::json!({"hit": false, "key": key}),
                    );
                    return Ok(Value::Object(map));
                }
            }
        }

        Ok(response)
    }

    async fn paginate(&self, args: Value) -> Result<Value, ToolError> {
        let profile = self
            .resolve_profile(args.get("profile_name"), &args)
            .await?;
        let mut auth = profile.auth.clone();
        if args.get("auth").is_some() {
            auth = normalize_auth_value(args.get("auth").unwrap_or(&Value::Null));
        }
        let auth_provider = if args.get("auth_provider").is_some() {
            args.get("auth_provider").cloned()
        } else {
            profile.auth_provider.clone()
        };
        let resolved_provider = self
            .resolve_auth_provider(auth_provider, profile.name.as_deref(), &args)
            .await?;
        if resolved_provider.is_some() {
            auth = resolved_provider;
        }

        let pagination =
            self.normalize_pagination(args.get("pagination"), profile.pagination.as_ref())?;
        let mut pages = Vec::new();
        let mut items = Vec::new();

        let mut cursor = pagination.start;
        let mut page_number = pagination.start;
        let mut offset = pagination.start;
        let mut next_url = args
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        for _ in 0..pagination.max_pages {
            let mut request_args = args.clone();
            if let Value::Object(map) = &mut request_args {
                map.remove("pagination");
            }

            match pagination.kind.as_str() {
                "page" => {
                    inject_query_param(&mut request_args, &pagination.param, page_number.into());
                    inject_query_param(
                        &mut request_args,
                        &pagination.size_param,
                        pagination.size.into(),
                    );
                }
                "offset" => {
                    inject_query_param(&mut request_args, &pagination.param, offset.into());
                    inject_query_param(
                        &mut request_args,
                        &pagination.size_param,
                        pagination.size.into(),
                    );
                }
                "cursor" => {
                    if cursor != 0 {
                        inject_query_param(&mut request_args, &pagination.param, cursor.into());
                    }
                    if !pagination.size_param.is_empty() {
                        inject_query_param(
                            &mut request_args,
                            &pagination.size_param,
                            pagination.size.into(),
                        );
                    }
                }
                "link" => {
                    if let Some(url) = next_url.clone() {
                        if let Value::Object(map) = &mut request_args {
                            map.insert("url".to_string(), Value::String(url));
                            map.remove("path");
                            map.remove("base_url");
                        }
                    } else {
                        break;
                    }
                }
                _ => {}
            }

            let response = self
                .request_with_retry(&request_args, &profile, auth.as_ref())
                .await?;
            pages.push(response.clone());

            if let Some(item_path) = pagination.item_path.as_deref() {
                let page_items =
                    get_path_value(&response, item_path, false, Some(Value::Array(Vec::new())))?;
                if let Some(arr) = page_items.as_array() {
                    if pagination.stop_on_empty && arr.is_empty() {
                        break;
                    }
                    items.extend(arr.iter().cloned());
                } else if pagination.stop_on_empty {
                    break;
                }
            }

            match pagination.kind.as_str() {
                "page" => {
                    page_number += 1;
                }
                "offset" => {
                    offset += pagination.size as i64;
                }
                "cursor" => {
                    let cursor_path = pagination.cursor_path.as_deref().ok_or_else(|| {
                        ToolError::invalid_params(
                            "pagination.cursor_path is required for cursor pagination",
                        )
                    })?;
                    let next_cursor =
                        get_path_value(&response, cursor_path, false, Some(Value::Null))?;
                    if next_cursor.is_null() {
                        cursor = 0;
                        break;
                    }
                    cursor = next_cursor.as_i64().unwrap_or(cursor);
                }
                "link" => {
                    let header = response
                        .get("headers")
                        .and_then(|v| v.get("link"))
                        .or_else(|| response.get("headers").and_then(|v| v.get("Link")))
                        .or_else(|| response.get("headers").and_then(|v| v.get("LINK")));
                    let link_header = header.and_then(|v| v.as_str()).unwrap_or("");
                    let links = parse_link_header(link_header);
                    let next = links.iter().find(|link| link.rel == pagination.link_rel);
                    if let Some(next) = next {
                        next_url = Some(next.url.clone());
                    } else {
                        break;
                    }
                }
                _ => {}
            }
        }

        let success = pages.iter().all(|page| {
            page.get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
        });
        let mut result = serde_json::json!({
            "success": success,
            "pages": pages,
            "page_count": pages.len(),
            "next_cursor": if pagination.kind == "cursor" { Value::Number(cursor.into()) } else { Value::Null },
        });
        if pagination.item_path.is_some() {
            if let Value::Object(map) = &mut result {
                map.insert("items".to_string(), Value::Array(items));
            }
        }
        Ok(result)
    }

    async fn download(&self, args: Value) -> Result<Value, ToolError> {
        let profile = self
            .resolve_profile(args.get("profile_name"), &args)
            .await?;
        let mut auth = profile.auth.clone();
        if args.get("auth").is_some() {
            auth = normalize_auth_value(args.get("auth").unwrap_or(&Value::Null));
        }
        let auth_provider = if args.get("auth_provider").is_some() {
            args.get("auth_provider").cloned()
        } else {
            profile.auth_provider.clone()
        };
        let resolved_provider = self
            .resolve_auth_provider(auth_provider, profile.name.as_deref(), &args)
            .await?;
        if resolved_provider.is_some() {
            auth = resolved_provider;
        }

        let policy = self.normalize_retry_policy(
            args.get("retry"),
            profile.retry.as_ref(),
            args.get("method"),
        );
        if !policy.enabled {
            return self.download_once(&args, &profile, auth.as_ref()).await;
        }

        let mut attempt = 0;
        let mut last_error: Option<ToolError> = None;
        while attempt < policy.max_attempts {
            attempt += 1;
            match self.download_once(&args, &profile, auth.as_ref()).await {
                Ok(response) => {
                    let should_retry = self.should_retry_response(&response, &policy);
                    if !should_retry || attempt >= policy.max_attempts {
                        let mut out = response;
                        if let Value::Object(map) = &mut out {
                            map.insert(
                                "attempts".to_string(),
                                Value::Number((attempt as u64).into()),
                            );
                            map.insert(
                                "retries".to_string(),
                                Value::Number(((attempt - 1) as u64).into()),
                            );
                        }
                        return Ok(out);
                    }
                    self.logger.warn(
                        "Retrying download",
                        Some(&serde_json::json!({"attempt": attempt})),
                    );
                    let delay = self.compute_retry_delay(attempt, &policy, Some(&response));
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                Err(err) => {
                    last_error = Some(err.clone());
                    if !policy.retry_on_network_error || attempt >= policy.max_attempts {
                        return Err(err);
                    }
                    let delay = self.compute_retry_delay(attempt, &policy, None);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| ToolError::retryable("Download failed after retries")))
    }

    async fn check_api(&self, args: Value) -> Result<Value, ToolError> {
        match self.request(merge_action(&args, "request", "GET")).await {
            Ok(result) => {
                let status = result.get("status").and_then(|v| v.as_u64()).unwrap_or(0) as i64;
                Ok(serde_json::json!({
                    "success": true,
                    "accessible": status < 500,
                    "status": status,
                    "response": result.get("data").cloned().or_else(|| result.get("body_base64").cloned()).unwrap_or(Value::Null),
                }))
            }
            Err(err) => Ok(serde_json::json!({
                "success": false,
                "accessible": false,
                "error": err.message,
            })),
        }
    }

    async fn smoke_http(&self, args: Value) -> Result<Value, ToolError> {
        let url =
            self.validation
                .ensure_string(args.get("url").unwrap_or(&Value::Null), "url", true)?;
        let expect_code = args
            .get("expect_code")
            .and_then(|v| v.as_i64())
            .unwrap_or(200);
        let follow_redirects = args
            .get("follow_redirects")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let insecure_ok = args
            .get("insecure_ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let max_bytes = std::cmp::min(
            read_positive_int(args.get("max_bytes")).unwrap_or(32 * 1024),
            256 * 1024,
        ) as usize;
        let timeout_ms = std::cmp::min(
            read_positive_int(args.get("timeout_ms")).unwrap_or(10_000),
            120_000,
        ) as u64;
        let started = Instant::now();

        let parsed = parse_url(&url)?;
        if parsed.username() != "" || parsed.password().is_some() {
            return Ok(serde_json::json!({
                "success": false,
                "url": url,
                "expect_code": expect_code,
                "error": "URL must not include credentials",
                "duration_ms": started.elapsed().as_millis(),
            }));
        }

        let client = self.get_client(false, insecure_ok)?;
        let mut current_url = parsed;
        let mut final_url = current_url.clone();
        let mut redirected = false;
        let mut status: i64 = 0;
        let mut capture: Option<BodyCapture> = None;

        for hop in 0..=10 {
            let elapsed = started.elapsed();
            if elapsed.as_millis() as u64 >= timeout_ms {
                return Ok(serde_json::json!({
                    "success": false,
                    "url": url,
                    "expect_code": expect_code,
                    "error": "timeout",
                    "duration_ms": elapsed.as_millis(),
                }));
            }
            let remaining = timeout_ms.saturating_sub(elapsed.as_millis() as u64);

            let response = client
                .request(Method::GET, current_url.clone())
                .header("accept", "*/*")
                .header("accept-encoding", "identity")
                .header("connection", "close")
                .timeout(Duration::from_millis(remaining))
                .send()
                .await;

            let response = match response {
                Ok(resp) => resp,
                Err(err) => {
                    return Ok(serde_json::json!({
                        "success": false,
                        "url": url,
                        "expect_code": expect_code,
                        "error": err.to_string(),
                        "duration_ms": started.elapsed().as_millis(),
                    }));
                }
            };

            status = response.status().as_u16() as i64;
            let headers = response.headers().clone();
            let location = headers
                .get("location")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());

            let body = read_response_body(response, max_bytes, None, None, None, None).await?;
            capture = Some(body);
            final_url = current_url.clone();

            let is_redirect = matches!(status, 301 | 302 | 303 | 307 | 308);
            if follow_redirects && is_redirect {
                if hop >= 10 {
                    return Ok(serde_json::json!({
                        "success": false,
                        "url": url,
                        "expect_code": expect_code,
                        "status": status,
                        "error": "Too many redirects",
                        "duration_ms": started.elapsed().as_millis(),
                    }));
                }
                let Some(location) = location else {
                    break;
                };
                let next = current_url
                    .join(&location)
                    .map_err(|_| ToolError::invalid_params("Redirect URL invalid"))?;
                if !scheme_allowed(next.scheme()) {
                    return Ok(serde_json::json!({
                        "success": false,
                        "url": url,
                        "expect_code": expect_code,
                        "status": status,
                        "error": format!("Redirected to unsupported protocol: {}", next.scheme()),
                        "duration_ms": started.elapsed().as_millis(),
                    }));
                }
                if next.username() != "" || next.password().is_some() {
                    return Ok(serde_json::json!({
                        "success": false,
                        "url": url,
                        "expect_code": expect_code,
                        "status": status,
                        "error": "Redirect URL must not include credentials",
                        "duration_ms": started.elapsed().as_millis(),
                    }));
                }
                current_url = next;
                redirected = true;
                continue;
            }
            break;
        }

        let capture = capture.unwrap_or(BodyCapture {
            buffer: Vec::new(),
            body_read_bytes: 0,
            body_captured_bytes: 0,
            body_truncated: false,
            body_ref: None,
            body_ref_truncated: None,
        });
        let body_preview = redact_text(
            String::from_utf8_lossy(&capture.buffer).as_ref(),
            usize::MAX,
            None,
        );
        Ok(serde_json::json!({
            "success": true,
            "ok": status == expect_code,
            "url": url,
            "final_url": final_url.to_string(),
            "redirected": redirected,
            "insecure_ok": insecure_ok,
            "follow_redirects": follow_redirects,
            "expect_code": expect_code,
            "status": status,
            "duration_ms": started.elapsed().as_millis(),
            "bytes": capture.body_read_bytes,
            "captured_bytes": capture.body_captured_bytes,
            "truncated": capture.body_truncated,
            "body_preview": body_preview,
        }))
    }

    async fn download_once(
        &self,
        args: &Value,
        profile: &ApiProfile,
        auth: Option<&Value>,
    ) -> Result<Value, ToolError> {
        let config = self.build_request_config(args, profile, auth, None)?;
        let raw_path = args
            .get("download_path")
            .or_else(|| args.get("file_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if raw_path.is_empty() {
            return Err(
                ToolError::invalid_params("download_path is required").with_hint(
                    "Provide args.download_path (or args.file_path) as a local filesystem path.",
                ),
            );
        }
        let file_path = expand_home_path(raw_path);
        let overwrite = args
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !overwrite && file_path.exists() {
            return Err(ToolError::conflict(format!(
                "Local path already exists: {}",
                file_path.display()
            ))
            .with_hint("Set overwrite=true to replace it."));
        }

        let client = self.get_client(true, false)?;
        let mut req = client.request(config.method.clone(), config.url.clone());
        req = req.headers(config.headers.clone());
        if let Some(body) = config.body {
            req = req.body(body);
        }
        if let Some(timeout_ms) = config.timeout_ms {
            req = req.timeout(Duration::from_millis(timeout_ms));
        }

        let started = Instant::now();
        let response = req.send().await.map_err(map_reqwest_error)?;
        let status = response.status();
        let headers = response.headers().clone();

        let tmp_path = file_path.with_extension("part");
        if let Some(parent) = file_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let mut file = tokio::fs::File::create(&tmp_path).await.map_err(|err| {
            ToolError::internal(format!("Failed to create download file: {}", err))
        })?;

        let mut stream = response.bytes_stream();
        let mut bytes: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_reqwest_error)?;
            bytes += chunk.len() as u64;
            file.write_all(&chunk).await.map_err(|err| {
                ToolError::internal(format!("Failed to write download chunk: {}", err))
            })?;
        }
        file.flush().await.ok();
        drop(file);
        tokio::fs::rename(&tmp_path, &file_path)
            .await
            .map_err(|err| ToolError::internal(format!("Failed to finalize download: {}", err)))?;

        let headers_map = headers_to_value(&headers);
        Ok(serde_json::json!({
            "success": status.is_success(),
            "method": config.method.as_str(),
            "url": config.url,
            "status": status.as_u16(),
            "statusText": status.canonical_reason().unwrap_or(""),
            "headers": headers_map,
            "file_path": file_path.display().to_string(),
            "bytes": bytes,
            "duration_ms": started.elapsed().as_millis(),
        }))
    }

    async fn request_with_retry(
        &self,
        args: &Value,
        profile: &ApiProfile,
        auth: Option<&Value>,
    ) -> Result<Value, ToolError> {
        let policy = self.normalize_retry_policy(
            args.get("retry"),
            profile.retry.as_ref(),
            args.get("method"),
        );
        if !policy.enabled {
            return self.request_once(args, profile, auth, None).await;
        }

        let mut attempt = 0;
        let mut last_error: Option<ToolError> = None;

        while attempt < policy.max_attempts {
            attempt += 1;
            match self.request_once(args, profile, auth, None).await {
                Ok(response) => {
                    if !self.should_retry_response(&response, &policy)
                        || attempt >= policy.max_attempts
                    {
                        let mut out = response;
                        if let Value::Object(map) = &mut out {
                            map.insert(
                                "attempts".to_string(),
                                Value::Number((attempt as u64).into()),
                            );
                            map.insert(
                                "retries".to_string(),
                                Value::Number(((attempt - 1) as u64).into()),
                            );
                        }
                        return Ok(out);
                    }
                    self.logger
                        .warn("HTTP retry", Some(&serde_json::json!({"attempt": attempt})));
                    let delay = self.compute_retry_delay(attempt, &policy, Some(&response));
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                Err(err) => {
                    last_error = Some(err.clone());
                    if !policy.retry_on_network_error || attempt >= policy.max_attempts {
                        return Err(err);
                    }
                    let delay = self.compute_retry_delay(attempt, &policy, None);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| ToolError::retryable("Request failed after retries")))
    }

    async fn request_once(
        &self,
        args: &Value,
        profile: &ApiProfile,
        auth: Option<&Value>,
        overrides: Option<RequestOverrides>,
    ) -> Result<Value, ToolError> {
        let config = self.build_request_config(args, profile, auth, overrides)?;
        let client = self.get_client(config.follow_redirects, config.insecure_ok)?;

        let mut req = client.request(config.method.clone(), config.url.clone());
        req = req.headers(config.headers.clone());
        if let Some(body) = config.body {
            req = req.body(body);
        }
        if let Some(timeout_ms) = config.timeout_ms {
            req = req.timeout(Duration::from_millis(timeout_ms));
        }

        let started = Instant::now();
        let response = req.send().await.map_err(map_reqwest_error)?;
        let status = response.status();
        let status_text = status.canonical_reason().unwrap_or("").to_string();
        let response_headers = response.headers().clone();

        let content_type = response_headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let response_type = args
            .get("response_type")
            .or_else(|| profile.data.get("response_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("auto")
            .to_lowercase();

        let max_capture_bytes = resolve_max_capture_bytes();
        let stream_mode = resolve_stream_to_artifact_mode();
        let context_root = stream_mode.and_then(|_| resolve_context_root());
        let trace_id = args.get("trace_id").and_then(|v| v.as_str());
        let span_id = args.get("span_id").and_then(|v| v.as_str());

        let capture = read_response_body(
            response,
            max_capture_bytes,
            stream_mode,
            context_root.as_ref(),
            trace_id,
            span_id,
        )
        .await?;

        let (data, data_truncated, body_base64, body_bytes) = match response_type.as_str() {
            "bytes" => {
                let base64 = base64::engine::general_purpose::STANDARD.encode(&capture.buffer);
                (
                    Value::Null,
                    Value::Null,
                    Some(base64),
                    Some(capture.buffer.len() as u64),
                )
            }
            "text" => {
                let text = String::from_utf8_lossy(&capture.buffer).to_string();
                (
                    Value::String(text),
                    Value::Bool(capture.body_truncated),
                    None,
                    None,
                )
            }
            "json" => {
                let text = String::from_utf8_lossy(&capture.buffer).to_string();
                if !capture.body_truncated {
                    if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                        (parsed, Value::Bool(false), None, None)
                    } else {
                        (Value::String(text), Value::Bool(false), None, None)
                    }
                } else {
                    (Value::String(text), Value::Bool(true), None, None)
                }
            }
            _ => {
                if content_type.contains("application/json") {
                    let text = String::from_utf8_lossy(&capture.buffer).to_string();
                    if !capture.body_truncated {
                        if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                            (parsed, Value::Bool(false), None, None)
                        } else {
                            (Value::String(text), Value::Bool(false), None, None)
                        }
                    } else {
                        (Value::String(text), Value::Bool(true), None, None)
                    }
                } else {
                    let text = String::from_utf8_lossy(&capture.buffer).to_string();
                    (
                        Value::String(text),
                        Value::Bool(capture.body_truncated),
                        None,
                        None,
                    )
                }
            }
        };

        let out = serde_json::json!({
            "success": status.is_success(),
            "method": config.method.as_str(),
            "url": config.url,
            "status": status.as_u16(),
            "statusText": status_text,
            "headers": headers_to_value(&response_headers),
            "duration_ms": started.elapsed().as_millis(),
            "data": data,
            "data_truncated": data_truncated,
            "body_base64": body_base64,
            "body_bytes": body_bytes,
            "body_read_bytes": capture.body_read_bytes,
            "body_captured_bytes": capture.body_captured_bytes,
            "body_truncated": capture.body_truncated,
            "body_ref": capture.body_ref,
            "body_ref_truncated": capture.body_ref_truncated,
        });

        Ok(out)
    }

    pub(crate) fn build_request_config(
        &self,
        args: &Value,
        profile: &ApiProfile,
        auth: Option<&Value>,
        overrides: Option<RequestOverrides>,
    ) -> Result<RequestConfig, ToolError> {
        let base_url = args
            .get("base_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                profile
                    .data
                    .get("base_url")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });
        let headers = merge_headers(
            profile.data.get("headers"),
            args.get("headers"),
            auth.and_then(|v| build_auth_headers(v).ok()),
        )?;

        let (body, content_type) = prepare_body(
            args.get("body").or_else(|| args.get("data")),
            args.get("body_type"),
            args.get("body_base64"),
            args.get("form"),
        )?;
        let mut headers = headers;
        if let Some(content_type) = content_type {
            if !headers.contains_key("Content-Type") && !headers.contains_key("content-type") {
                headers.insert("Content-Type".to_string(), content_type);
            }
        }

        let url = build_url(
            base_url.as_deref(),
            args.get("path"),
            args.get("query"),
            args.get("url"),
        )?;
        let method = args
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET")
            .to_uppercase();
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .or_else(|| profile.data.get("timeout_ms").and_then(|v| v.as_u64()))
            .unwrap_or(network_constants::TIMEOUT_API_REQUEST_MS);
        let redirect_setting = args
            .get("redirect")
            .or_else(|| profile.data.get("redirect"))
            .and_then(|v| v.as_str())
            .unwrap_or("follow")
            .to_lowercase();
        let follow_redirects = args
            .get("follow_redirects")
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| redirect_setting == "follow");

        let mut config = RequestConfig {
            url,
            method: Method::from_bytes(method.as_bytes())
                .map_err(|_| ToolError::invalid_params("Invalid HTTP method"))?,
            headers: headers_to_headermap(&headers)?,
            headers_raw: headers,
            body,
            timeout_ms: Some(timeout_ms),
            follow_redirects,
            insecure_ok: args
                .get("insecure_ok")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };

        if let Some(overrides) = overrides {
            if let Some(url) = overrides.url {
                config.url = url;
            }
            if let Some(method) = overrides.method {
                config.method = method;
            }
            if let Some(headers) = overrides.headers {
                config.headers = headers;
            }
            if let Some(body) = overrides.body {
                config.body = Some(body);
            }
            if let Some(timeout) = overrides.timeout_ms {
                config.timeout_ms = Some(timeout);
            }
        }

        Ok(config)
    }

    pub(crate) async fn resolve_profile(
        &self,
        profile_name: Option<&Value>,
        args: &Value,
    ) -> Result<ApiProfile, ToolError> {
        let mut profile_name = profile_name.and_then(|v| v.as_str()).map(|s| s.to_string());
        if profile_name.is_none() {
            if let Some(resolver) = &self.project_resolver {
                if let Ok(context) = resolver.resolve_context(args).await {
                    if let Some(profile) = context
                        .as_ref()
                        .and_then(|v| v.get("target"))
                        .and_then(|v| v.get("api_profile"))
                        .and_then(|v| v.as_str())
                    {
                        profile_name =
                            Some(self.validation.ensure_identifier(profile, "profile_name")?);
                    }
                }
            }
        }

        if profile_name.is_none() {
            let profiles = self.profile_service.list_profiles(Some(API_PROFILE_TYPE))?;
            if let Some(arr) = profiles.as_array() {
                if arr.len() == 1 {
                    profile_name = arr[0]
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
            }
        }

        if profile_name.is_none() {
            return Ok(ApiProfile {
                name: None,
                data: Default::default(),
                auth: None,
                auth_provider: None,
                retry: None,
                pagination: None,
                cache: None,
            });
        }

        let profile = self
            .profile_service
            .get_profile(profile_name.as_deref().unwrap(), Some(API_PROFILE_TYPE))?;
        let data = profile
            .get("data")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let secrets = profile
            .get("secrets")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let mut auth = merge_auth(data.get("auth"), Some(&Value::Object(secrets.clone())));
        let mut auth_provider = merge_auth_provider(
            data.get("auth_provider"),
            Some(&Value::Object(secrets.clone())),
        );

        if let Some(resolver) = &self.secret_ref_resolver {
            if let Some(auth_value) = auth.clone() {
                auth = Some(resolver.resolve_deep(&auth_value, args).await?);
            }
            if let Some(provider) = auth_provider.clone() {
                auth_provider = Some(resolver.resolve_deep(&provider, args).await?);
            }
        }

        Ok(ApiProfile {
            name: profile_name,
            data,
            auth,
            auth_provider,
            retry: profile.get("data").and_then(|v| v.get("retry")).cloned(),
            pagination: profile
                .get("data")
                .and_then(|v| v.get("pagination"))
                .cloned(),
            cache: profile.get("data").and_then(|v| v.get("cache")).cloned(),
        })
    }

    pub(crate) fn normalize_retry_policy(
        &self,
        request: Option<&Value>,
        profile: Option<&Value>,
        method: Option<&Value>,
    ) -> RetryPolicy {
        let mut policy = RetryPolicy {
            enabled: true,
            max_attempts: retry_constants::MAX_ATTEMPTS,
            base_delay_ms: retry_constants::BASE_DELAY_MS,
            max_delay_ms: retry_constants::MAX_DELAY_MS,
            jitter: retry_constants::JITTER,
            status_codes: retry_constants::STATUS_CODES.to_vec(),
            methods: None,
            retry_on_network_error: true,
            respect_retry_after: true,
        };

        if let Some(profile) = profile {
            apply_retry_policy(&mut policy, profile);
        }
        if let Some(request) = request {
            apply_retry_policy(&mut policy, request);
        }

        if let Some(method) = method.and_then(|v| v.as_str()) {
            if let Some(methods) = policy.methods.as_ref() {
                if !methods.iter().any(|m| m.eq_ignore_ascii_case(method)) {
                    policy.enabled = false;
                }
            }
        }

        policy
    }

    fn normalize_cache_policy(
        &self,
        request: Option<&Value>,
        profile: Option<&Value>,
    ) -> CachePolicy {
        let mut policy = CachePolicy {
            enabled: false,
            ttl_ms: Some(cache_constants::DEFAULT_TTL_MS),
            cache_errors: false,
            key: None,
        };

        if let Some(profile) = profile {
            apply_cache_policy(&mut policy, profile);
        }
        if let Some(request) = request {
            apply_cache_policy(&mut policy, request);
        }

        policy
    }

    fn normalize_pagination(
        &self,
        request: Option<&Value>,
        profile: Option<&Value>,
    ) -> Result<PaginationConfig, ToolError> {
        let mut merged = serde_json::Map::new();
        if let Some(profile) = profile.and_then(|v| v.as_object()) {
            for (k, v) in profile {
                merged.insert(k.clone(), v.clone());
            }
        }
        if let Some(request) = request.and_then(|v| v.as_object()) {
            for (k, v) in request {
                merged.insert(k.clone(), v.clone());
            }
        }

        let kind = merged
            .get("type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase())
            .ok_or_else(|| ToolError::invalid_params("pagination.type is required"))?;

        let param = merged
            .get("param")
            .or_else(|| merged.get("cursor_param"))
            .and_then(|v| v.as_str())
            .unwrap_or("page")
            .to_string();
        let size_param = merged
            .get("size_param")
            .and_then(|v| v.as_str())
            .unwrap_or("limit")
            .to_string();
        let size = merged
            .get("size")
            .or_else(|| merged.get("page_size"))
            .and_then(|v| v.as_u64())
            .unwrap_or(pagination_constants::PAGE_SIZE as u64) as usize;
        let start = merged
            .get("start")
            .and_then(|v| v.as_i64())
            .unwrap_or(if kind == "page" { 1 } else { 0 });
        let max_pages = merged
            .get("max_pages")
            .and_then(|v| v.as_u64())
            .unwrap_or(pagination_constants::MAX_PAGES as u64) as usize;
        let item_path = merged
            .get("item_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let cursor_path = merged
            .get("cursor_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let link_rel = merged
            .get("link_rel")
            .and_then(|v| v.as_str())
            .unwrap_or("next")
            .to_string();
        let stop_on_empty = merged
            .get("stop_on_empty")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        Ok(PaginationConfig {
            kind,
            param,
            size_param,
            size,
            start,
            max_pages,
            item_path,
            cursor_path,
            link_rel,
            stop_on_empty,
        })
    }

    pub(crate) fn should_retry_response(&self, response: &Value, policy: &RetryPolicy) -> bool {
        if !policy.enabled {
            return false;
        }
        let status = response.get("status").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
        policy.status_codes.contains(&status)
    }

    pub(crate) fn compute_retry_delay(
        &self,
        attempt: usize,
        policy: &RetryPolicy,
        response: Option<&Value>,
    ) -> u64 {
        let base = policy.base_delay_ms;
        let factor: f64 = 2.0;
        let max_delay = policy.max_delay_ms;
        let jitter = policy.jitter;
        let mut delay = (base as f64) * factor.powi((attempt.saturating_sub(1)) as i32);
        if delay > max_delay as f64 {
            delay = max_delay as f64;
        }
        if jitter > 0.0 {
            let delta = delay * jitter;
            delay = delay - delta + rand::random::<f64>() * delta * 2.0;
        }

        if policy.respect_retry_after {
            if let Some(response) = response {
                if let Some(headers) = response.get("headers").and_then(|v| v.as_object()) {
                    if let Some(retry_after) = headers
                        .get("retry-after")
                        .or_else(|| headers.get("Retry-After"))
                        .and_then(|v| v.as_str())
                    {
                        if let Ok(parsed) = retry_after.parse::<u64>() {
                            if parsed > delay as u64 {
                                delay = parsed as f64;
                            }
                        }
                    }
                }
            }
        }

        delay.max(0.0) as u64
    }

    pub(crate) async fn resolve_auth_provider(
        &self,
        provider: Option<Value>,
        profile_name: Option<&str>,
        args: &Value,
    ) -> Result<Option<Value>, ToolError> {
        let Some(mut provider) = provider else {
            return Ok(None);
        };
        if let Some(resolver) = &self.secret_ref_resolver {
            provider = resolver.resolve_deep(&provider, args).await?;
        }

        let provider_obj = provider.as_object().cloned().unwrap_or_default();
        let mut kind = provider_obj
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if kind.is_empty() {
            if provider_obj.get("command").is_some() || provider_obj.get("exec").is_some() {
                kind = "exec".to_string();
            } else if provider_obj.get("token_url").is_some() {
                kind = "oauth2".to_string();
            } else if provider_obj.get("token").is_some() || provider_obj.get("auth").is_some() {
                kind = "static".to_string();
            }
        }

        if kind == "static" {
            if let Some(auth) = provider_obj.get("auth") {
                return Ok(Some(auth.clone()));
            }
            if let Some(token) = provider_obj.get("token").and_then(|v| v.as_str()) {
                return Ok(Some(serde_json::json!({"type": "bearer", "token": token})));
            }
            return Ok(None);
        }

        if kind == "exec" {
            let exec = provider_obj
                .get("exec")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            let command = exec
                .get("command")
                .or_else(|| provider_obj.get("command"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ToolError::invalid_params("auth_provider.exec.command is required")
                })?;
            let args_list = exec
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let timeout_ms = exec.get("timeout_ms").and_then(|v| v.as_u64());
            let mut cmd = tokio::process::Command::new(command);
            cmd.args(args_list);
            if let Some(cwd) = exec.get("cwd").and_then(|v| v.as_str()) {
                cmd.current_dir(cwd);
            }
            if let Some(env) = exec.get("env").and_then(|v| v.as_object()) {
                for (k, v) in env {
                    if let Some(s) = v.as_str() {
                        cmd.env(k, s);
                    }
                }
            }
            let output = if let Some(timeout_ms) = timeout_ms {
                tokio::time::timeout(Duration::from_millis(timeout_ms), cmd.output())
                    .await
                    .map_err(|_| ToolError::timeout("auth_provider.exec timed out"))??
            } else {
                cmd.output()
                    .await
                    .map_err(|err| ToolError::internal(err.to_string()))?
            };
            if !output.status.success() {
                return Err(ToolError::internal("auth_provider.exec failed"));
            }
            let format = exec
                .get("format")
                .or_else(|| provider_obj.get("format"))
                .and_then(|v| v.as_str())
                .unwrap_or("raw")
                .to_lowercase();
            let mut token = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if format == "json" {
                let parsed: Value = serde_json::from_str(&token).map_err(|_| {
                    ToolError::invalid_params("auth_provider.exec returned invalid JSON")
                })?;
                let token_path = exec
                    .get("token_path")
                    .or_else(|| provider_obj.get("token_path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("token");
                token = get_path_value(&parsed, token_path, true, None)?
                    .as_str()
                    .unwrap_or("")
                    .to_string();
            }
            if token.is_empty() {
                return Err(ToolError::invalid_params(
                    "auth_provider.exec did not return a token",
                ));
            }
            return Ok(Some(auth_from_token(&provider_obj, &token)));
        }

        if kind == "oauth2" {
            let token_url = provider_obj
                .get("token_url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::invalid_params("auth_provider.token_url is required"))?;
            let client_id = provider_obj
                .get("client_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::invalid_params("auth_provider.client_id is required"))?;
            let client_secret = provider_obj
                .get("client_secret")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ToolError::invalid_params("auth_provider.client_secret is required")
                })?;

            let cache_key = provider_obj
                .get("cache_key")
                .and_then(|v| v.as_str())
                .map(|s| format!("{}:{}", profile_name.unwrap_or("inline"), s))
                .unwrap_or_else(|| format!("{}:{}", profile_name.unwrap_or("inline"), token_url));
            if let Some(token) = self.get_cached_token(&cache_key) {
                return Ok(Some(auth_from_token(&provider_obj, &token)));
            }

            let grant_type = provider_obj
                .get("grant_type")
                .and_then(|v| v.as_str())
                .unwrap_or("client_credentials");
            let mut payload = HashMap::new();
            payload.insert("grant_type", grant_type.to_string());
            payload.insert("client_id", client_id.to_string());
            payload.insert("client_secret", client_secret.to_string());
            if let Some(scope) = provider_obj.get("scope").and_then(|v| v.as_str()) {
                payload.insert("scope", scope.to_string());
            }
            if let Some(audience) = provider_obj.get("audience").and_then(|v| v.as_str()) {
                payload.insert("audience", audience.to_string());
            }
            if grant_type == "refresh_token" {
                let refresh = provider_obj
                    .get("refresh_token")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::invalid_params("auth_provider.refresh_token is required")
                    })?;
                payload.insert("refresh_token", refresh.to_string());
            }
            if let Some(extra) = provider_obj.get("extra").and_then(|v| v.as_object()) {
                for (k, v) in extra {
                    payload.insert(k.as_str(), v.as_str().unwrap_or(&v.to_string()).to_string());
                }
            }

            let client = self.get_client(true, false)?;
            let response = client
                .post(token_url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .form(&payload)
                .send()
                .await
                .map_err(map_reqwest_error)?;
            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                let redacted = redact_text(&text, 16 * 1024, None);
                return Err(ToolError::invalid_params(format!(
                    "OAuth2 token request failed ({})",
                    status.as_u16()
                ))
                .with_details(serde_json::json!({
                    "status": status.as_u16(),
                    "body": redacted,
                })));
            }
            let token_payload: Value = response
                .json()
                .await
                .map_err(|_| ToolError::internal("OAuth2 token response invalid"))?;
            let token_path = provider_obj
                .get("token_path")
                .and_then(|v| v.as_str())
                .unwrap_or("access_token");
            let token = get_path_value(&token_payload, token_path, true, None)?
                .as_str()
                .unwrap_or("")
                .to_string();
            if token.is_empty() {
                return Err(ToolError::invalid_params(
                    "OAuth2 token not found in response",
                ));
            }
            if let Some(expires_in) = token_payload.get("expires_in").and_then(|v| v.as_i64()) {
                let ttl_ms = expires_in.max(0) as u64 * 1000;
                let buffer = provider_obj
                    .get("expiry_buffer_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(30_000);
                if ttl_ms > buffer {
                    self.set_cached_token(&cache_key, &token, Some(ttl_ms - buffer));
                }
            }
            return Ok(Some(auth_from_token(&provider_obj, &token)));
        }

        Ok(None)
    }

    pub(crate) fn get_client(
        &self,
        follow_redirects: bool,
        insecure_ok: bool,
    ) -> Result<Client, ToolError> {
        let key = (follow_redirects, insecure_ok);
        if let Ok(mut guard) = self.clients.lock() {
            if let Some(existing) = guard.get(&key) {
                return Ok(existing.clone());
            }
            let mut builder = Client::builder();
            if follow_redirects {
                builder = builder.redirect(reqwest::redirect::Policy::limited(10));
            } else {
                builder = builder.redirect(reqwest::redirect::Policy::none());
            }
            if insecure_ok {
                builder = builder.danger_accept_invalid_certs(true);
            }
            let client = builder.build().map_err(|err| {
                ToolError::internal(format!("Failed to build HTTP client: {}", err))
            })?;
            guard.insert(key, client.clone());
            return Ok(client);
        }
        Err(ToolError::internal("Failed to access HTTP client cache"))
    }

    fn get_cached_token(&self, key: &str) -> Option<String> {
        let guard = self.token_cache.lock().ok()?;
        let entry = guard.get(key)?;
        if let Some(expires_at) = entry.expires_at {
            if Instant::now() >= expires_at {
                return None;
            }
        }
        Some(entry.token.clone())
    }

    fn set_cached_token(&self, key: &str, token: &str, ttl_ms: Option<u64>) {
        let expires_at = ttl_ms.map(|ttl| Instant::now() + Duration::from_millis(ttl));
        if let Ok(mut guard) = self.token_cache.lock() {
            guard.insert(
                key.to_string(),
                CachedToken {
                    token: token.to_string(),
                    expires_at,
                },
            );
        }
    }
}

pub(crate) struct RequestConfig {
    pub(crate) url: String,
    pub(crate) method: Method,
    pub(crate) headers: HeaderMap,
    pub(crate) headers_raw: HashMap<String, String>,
    pub(crate) body: Option<reqwest::Body>,
    pub(crate) timeout_ms: Option<u64>,
    pub(crate) follow_redirects: bool,
    pub(crate) insecure_ok: bool,
}

pub(crate) struct RequestOverrides {
    url: Option<String>,
    method: Option<Method>,
    headers: Option<HeaderMap>,
    body: Option<reqwest::Body>,
    timeout_ms: Option<u64>,
}

#[derive(Clone, Debug)]
struct LinkHeader {
    url: String,
    rel: String,
}

fn parse_link_header(header: &str) -> Vec<LinkHeader> {
    header
        .split(',')
        .filter_map(|part| {
            let trimmed = part.trim();
            let mut url = None;
            let mut rel = None;
            for piece in trimmed.split(';') {
                let piece = piece.trim();
                if piece.starts_with('<') && piece.ends_with('>') {
                    url = Some(piece.trim_matches(&['<', '>'][..]).to_string());
                } else if piece.starts_with("rel=") {
                    let value = piece.trim_start_matches("rel=").trim_matches('"');
                    rel = Some(value.to_string());
                }
            }
            if let (Some(url), Some(rel)) = (url, rel) {
                Some(LinkHeader { url, rel })
            } else {
                None
            }
        })
        .collect()
}

fn read_positive_int(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    if let Some(n) = value.as_u64() {
        return Some(n);
    }
    if let Some(s) = value.as_str() {
        return s.parse::<u64>().ok().filter(|v| *v > 0);
    }
    None
}

fn resolve_max_capture_bytes() -> usize {
    let raw = std::env::var("INFRA_API_MAX_CAPTURE_BYTES")
        .or_else(|_| std::env::var("INFRA_MAX_CAPTURE_BYTES"))
        .ok();
    raw.and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(256 * 1024)
}

fn resolve_stream_to_artifact_mode() -> Option<StreamMode> {
    let raw = std::env::var("INFRA_API_STREAM_TO_ARTIFACT")
        .or_else(|_| std::env::var("INFRA_STREAM_TO_ARTIFACT"))
        .ok()?;
    let normalized = raw.trim().to_lowercase();
    if normalized.is_empty() {
        return None;
    }
    match normalized.as_str() {
        "full" => Some(StreamMode::Full),
        "capped" => Some(StreamMode::Capped),
        _ => {
            if normalized == "1" || normalized == "true" || normalized == "yes" {
                Some(StreamMode::Capped)
            } else {
                None
            }
        }
    }
}

async fn read_response_body(
    response: reqwest::Response,
    max_capture_bytes: usize,
    stream_mode: Option<StreamMode>,
    context_root: Option<&std::path::PathBuf>,
    trace_id: Option<&str>,
    span_id: Option<&str>,
) -> Result<BodyCapture, ToolError> {
    let mut read_bytes: u64 = 0;
    let mut captured: usize = 0;
    let mut truncated = false;
    let mut preview = Vec::new();

    let mut body_ref = None;
    let mut body_ref_truncated = None;
    let mut writer = None;
    let mut artifact_written = 0usize;
    let artifact_limit = match stream_mode {
        Some(StreamMode::Full) => usize::MAX,
        Some(StreamMode::Capped) => max_capture_bytes,
        None => 0,
    };

    if artifact_limit > 0 {
        if let Some(root) = context_root {
            let filename = format!("api-body-{}.bin", uuid::Uuid::new_v4());
            if let Ok(reference) = build_tool_call_file_ref(trace_id, span_id, &filename) {
                if let Ok(stream) = create_artifact_write_stream(root, &reference).await {
                    writer = Some((reference, stream));
                }
            }
        }
    }

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(map_reqwest_error)?;
        read_bytes += chunk.len() as u64;

        if captured < max_capture_bytes {
            let remaining = max_capture_bytes - captured;
            if chunk.len() <= remaining {
                preview.extend_from_slice(&chunk);
                captured += chunk.len();
            } else {
                preview.extend_from_slice(&chunk[..remaining]);
                captured += remaining;
                truncated = true;
            }
        } else {
            truncated = true;
        }

        if let Some((_, ref mut stream)) = writer {
            if artifact_written < artifact_limit {
                let remaining = artifact_limit - artifact_written;
                let slice = if chunk.len() <= remaining {
                    &chunk[..]
                } else {
                    &chunk[..remaining]
                };
                if !slice.is_empty() {
                    stream.write(slice).await?;
                    artifact_written += slice.len();
                }
                if slice.len() < chunk.len() {
                    body_ref_truncated = Some(true);
                }
            } else if artifact_limit != usize::MAX {
                body_ref_truncated = Some(true);
            }
        }

        if captured >= max_capture_bytes && artifact_limit == 0 {
            break;
        }
    }

    if let Some((reference, stream)) = writer {
        if artifact_written > 0 {
            let written = stream.finalize().await?;
            body_ref = Some(serde_json::json!({
                "uri": written.uri,
                "rel": written.rel,
                "bytes": written.bytes,
            }));
            body_ref_truncated = body_ref_truncated.or(Some(false));
            let _ = reference;
        } else {
            let _ = stream.abort().await;
        }
    }

    Ok(BodyCapture {
        buffer: preview,
        body_read_bytes: read_bytes,
        body_captured_bytes: captured as u64,
        body_truncated: truncated,
        body_ref,
        body_ref_truncated,
    })
}

pub(crate) fn map_reqwest_error(err: reqwest::Error) -> ToolError {
    if err.is_timeout() {
        return ToolError::timeout("HTTP request timed out");
    }
    ToolError::retryable(err.to_string())
}

pub(crate) fn normalize_auth_value(raw: &Value) -> Option<Value> {
    if raw.is_null() {
        return None;
    }
    if let Some(text) = raw.as_str() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.to_lowercase().starts_with("bearer ")
            || trimmed.to_lowercase().starts_with("basic ")
        {
            return Some(serde_json::json!({"type": "raw", "value": trimmed}));
        }
        return Some(serde_json::json!({"type": "bearer", "token": trimmed}));
    }
    if raw.is_object() {
        return Some(raw.clone());
    }
    None
}

fn split_auth(auth: Option<&Value>) -> (Option<Value>, Option<serde_json::Map<String, Value>>) {
    let auth = auth.and_then(normalize_auth_value);
    let Some(Value::Object(mut auth_map)) = auth else {
        return (None, None);
    };

    let mut secrets = serde_json::Map::new();
    if let Some(token) = auth_map.remove("token") {
        secrets.insert("auth_token".to_string(), token);
    }
    if let Some(password) = auth_map.remove("password") {
        secrets.insert("auth_password".to_string(), password);
    }
    if let Some(header_value) = auth_map.remove("header_value") {
        secrets.insert("auth_header_value".to_string(), header_value);
    }
    if let Some(value) = auth_map.remove("value") {
        secrets.insert("auth_value".to_string(), value);
    }

    (
        Some(Value::Object(auth_map)),
        if secrets.is_empty() {
            None
        } else {
            Some(secrets)
        },
    )
}

fn merge_auth(data_auth: Option<&Value>, secrets: Option<&Value>) -> Option<Value> {
    let mut auth = data_auth.and_then(normalize_auth_value);
    let Some(Value::Object(map)) = auth.as_mut() else {
        return auth;
    };
    let secrets = secrets.and_then(|v| v.as_object());
    if let Some(secrets) = secrets {
        if let Some(token) = secrets.get("auth_token") {
            map.insert("token".to_string(), token.clone());
        }
        if let Some(password) = secrets.get("auth_password") {
            map.insert("password".to_string(), password.clone());
        }
        if let Some(header_value) = secrets.get("auth_header_value") {
            map.insert("header_value".to_string(), header_value.clone());
        }
        if let Some(value) = secrets.get("auth_value") {
            map.insert("value".to_string(), value.clone());
        }
    }
    auth
}

fn split_auth_provider(
    provider: Option<&Value>,
) -> (Option<Value>, Option<serde_json::Map<String, Value>>) {
    let Some(Value::Object(mut provider_map)) = provider.cloned() else {
        return (None, None);
    };
    let mut secrets = serde_json::Map::new();
    if let Some(secret) = provider_map.remove("client_secret") {
        secrets.insert("auth_provider_client_secret".to_string(), secret);
    }
    if let Some(secret) = provider_map.remove("refresh_token") {
        secrets.insert("auth_provider_refresh_token".to_string(), secret);
    }
    if let Some(exec) = provider_map.get("exec").and_then(|v| v.as_object()) {
        if let Some(env) = exec.get("env") {
            if let Ok(serialized) = serde_json::to_string(env) {
                secrets.insert(
                    "auth_provider_exec_env".to_string(),
                    Value::String(serialized),
                );
            }
        }
    }
    (
        Some(Value::Object(provider_map)),
        if secrets.is_empty() {
            None
        } else {
            Some(secrets)
        },
    )
}

fn merge_auth_provider(provider: Option<&Value>, secrets: Option<&Value>) -> Option<Value> {
    let Some(Value::Object(mut provider_map)) = provider.cloned() else {
        return provider.cloned();
    };
    let secrets = secrets.and_then(|v| v.as_object());
    if let Some(secrets) = secrets {
        if let Some(secret) = secrets.get("auth_provider_client_secret") {
            provider_map.insert("client_secret".to_string(), secret.clone());
        }
        if let Some(secret) = secrets.get("auth_provider_refresh_token") {
            provider_map.insert("refresh_token".to_string(), secret.clone());
        }
        if let Some(raw) = secrets
            .get("auth_provider_exec_env")
            .and_then(|v| v.as_str())
        {
            if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                let mut exec = provider_map
                    .get("exec")
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();
                exec.insert("env".to_string(), parsed);
                provider_map.insert("exec".to_string(), Value::Object(exec));
            }
        }
    }
    Some(Value::Object(provider_map))
}

fn auth_from_token(provider: &serde_json::Map<String, Value>, token: &str) -> Value {
    if let Some(header_name) = provider.get("header_name").and_then(|v| v.as_str()) {
        return serde_json::json!({"type": "header", "header_name": header_name, "header_value": token});
    }
    let scheme = provider
        .get("scheme")
        .and_then(|v| v.as_str())
        .unwrap_or("bearer")
        .to_lowercase();
    if scheme == "raw" || scheme == "basic" {
        return serde_json::json!({"type": "raw", "value": token});
    }
    serde_json::json!({"type": "bearer", "token": token})
}

fn build_auth_headers(auth: &Value) -> Result<HashMap<String, String>, ToolError> {
    let Some(auth) = normalize_auth_value(auth) else {
        return Ok(HashMap::new());
    };
    let Some(obj) = auth.as_object() else {
        return Ok(HashMap::new());
    };
    let typ = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let mut headers = HashMap::new();
    match typ {
        "raw" => {
            if let Some(value) = obj.get("value").and_then(|v| v.as_str()) {
                headers.insert("Authorization".to_string(), value.to_string());
            }
        }
        "bearer" => {
            if let Some(token) = obj.get("token").and_then(|v| v.as_str()) {
                let value = if token.to_lowercase().starts_with("bearer ") {
                    token.to_string()
                } else {
                    format!("Bearer {}", token)
                };
                headers.insert("Authorization".to_string(), value);
            }
        }
        "basic" => {
            let username = obj.get("username").and_then(|v| v.as_str()).unwrap_or("");
            let password = obj.get("password").and_then(|v| v.as_str()).unwrap_or("");
            let encoded = base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", username, password));
            headers.insert("Authorization".to_string(), format!("Basic {}", encoded));
        }
        "header" => {
            if let Some(name) = obj.get("header_name").and_then(|v| v.as_str()) {
                if let Some(value) = obj.get("header_value").and_then(|v| v.as_str()) {
                    headers.insert(name.to_string(), value.to_string());
                }
            }
        }
        _ => {}
    }
    Ok(headers)
}

fn merge_headers(
    profile_headers: Option<&Value>,
    request_headers: Option<&Value>,
    auth_headers: Option<HashMap<String, String>>,
) -> Result<HashMap<String, String>, ToolError> {
    let mut merged = HashMap::new();
    merged.insert(
        "User-Agent".to_string(),
        "infra-api-client/7.0.1".to_string(),
    );
    merged.insert(
        "Accept".to_string(),
        "application/json, text/plain, */*".to_string(),
    );

    if let Some(Value::Object(map)) = profile_headers {
        for (k, v) in map {
            if let Some(s) = v.as_str() {
                merged.insert(k.to_string(), s.to_string());
            } else if !v.is_null() {
                merged.insert(k.to_string(), v.to_string());
            }
        }
    }

    if let Some(Value::Object(map)) = request_headers {
        for (k, v) in map {
            if let Some(s) = v.as_str() {
                merged.insert(k.to_string(), s.to_string());
            } else if !v.is_null() {
                merged.insert(k.to_string(), v.to_string());
            }
        }
    }

    if let Some(auth) = auth_headers {
        for (k, v) in auth {
            merged.insert(k, v);
        }
    }

    Ok(merged)
}

fn headers_to_headermap(headers: &HashMap<String, String>) -> Result<HeaderMap, ToolError> {
    let mut map = HeaderMap::new();
    for (key, value) in headers {
        let name = HeaderName::from_bytes(key.as_bytes())
            .map_err(|_| ToolError::invalid_params("Invalid header name"))?;
        let val = HeaderValue::from_str(value)
            .map_err(|_| ToolError::invalid_params("Invalid header value"))?;
        map.insert(name, val);
    }
    Ok(map)
}

fn headers_to_value(headers: &HeaderMap) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in headers {
        if let Ok(text) = value.to_str() {
            map.insert(key.as_str().to_string(), Value::String(text.to_string()));
        }
    }
    Value::Object(map)
}

fn prepare_body(
    body: Option<&Value>,
    body_type: Option<&Value>,
    body_base64: Option<&Value>,
    form: Option<&Value>,
) -> Result<(Option<reqwest::Body>, Option<String>), ToolError> {
    if let Some(raw) = body_base64.and_then(|v| v.as_str()) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(raw.as_bytes())
            .map_err(|_| ToolError::invalid_params("body_base64 must be valid base64"))?;
        return Ok((
            Some(reqwest::Body::from(bytes)),
            Some("application/octet-stream".to_string()),
        ));
    }

    if let Some(Value::Object(form_map)) = form {
        let mut params = Vec::new();
        for (k, v) in form_map {
            let rendered = v
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| v.to_string());
            params.push((k.clone(), rendered));
        }
        let encoded = serde_urlencoded::to_string(params)
            .map_err(|_| ToolError::invalid_params("form must be a simple object"))?;
        return Ok((
            Some(reqwest::Body::from(encoded)),
            Some("application/x-www-form-urlencoded".to_string()),
        ));
    }

    let body_type = body_type
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    if let Some(body) = body {
        if body.is_null() {
            return Ok((None, None));
        }
        if body_type == "json" || body.is_object() || body.is_array() {
            let text = serde_json::to_string(body)
                .map_err(|_| ToolError::invalid_params("body must be JSON-serializable"))?;
            return Ok((
                Some(reqwest::Body::from(text)),
                Some("application/json".to_string()),
            ));
        }
        if body_type == "text" {
            let text = body
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| body.to_string());
            return Ok((
                Some(reqwest::Body::from(text)),
                Some("text/plain".to_string()),
            ));
        }
        let text = body
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| body.to_string());
        return Ok((Some(reqwest::Body::from(text)), None));
    }

    Ok((None, None))
}

fn build_url(
    base_url: Option<&str>,
    path: Option<&Value>,
    query: Option<&Value>,
    url: Option<&Value>,
) -> Result<String, ToolError> {
    let mut url = if let Some(url) = url.and_then(|v| v.as_str()) {
        Url::parse(url).map_err(|_| ToolError::invalid_params("Invalid url"))?
    } else {
        let base = base_url
            .ok_or_else(|| ToolError::invalid_params("base_url or url must be provided"))?;
        let base_url =
            Url::parse(base).map_err(|_| ToolError::invalid_params("Invalid base_url"))?;
        if let Some(path) = path.and_then(|v| v.as_str()) {
            base_url
                .join(path)
                .map_err(|_| ToolError::invalid_params("Invalid path"))?
        } else {
            base_url
        }
    };

    if !scheme_allowed(url.scheme()) {
        return Err(ToolError::invalid_params(
            "Only http/https URLs are supported",
        ));
    }

    match query {
        Some(Value::Object(map)) => {
            for (k, v) in map {
                if let Some(arr) = v.as_array() {
                    for item in arr {
                        url.query_pairs_mut().append_pair(k, &item.to_string());
                    }
                } else if !v.is_null() {
                    url.query_pairs_mut().append_pair(k, &v.to_string());
                }
            }
        }
        Some(Value::String(raw)) => {
            url.set_query(Some(raw));
        }
        _ => {}
    }

    Ok(url.to_string())
}

fn inject_query_param(target: &mut Value, key: &str, value: Value) {
    if let Value::Object(map) = target {
        let entry = map
            .entry("query".to_string())
            .or_insert(Value::Object(Default::default()));
        if let Value::Object(query) = entry {
            query.insert(key.to_string(), value);
        }
    }
}

fn parse_url(raw: &str) -> Result<Url, ToolError> {
    let parsed = Url::parse(raw).map_err(|_| ToolError::invalid_params("Invalid URL"))?;
    if !scheme_allowed(parsed.scheme()) {
        return Err(ToolError::invalid_params(
            "Only http/https URLs are supported",
        ));
    }
    Ok(parsed)
}

fn scheme_allowed(scheme: &str) -> bool {
    let normalized = scheme.trim_end_matches(':');
    ALLOWED_HTTP
        .iter()
        .any(|allowed| allowed.trim_end_matches(':') == normalized)
}

fn apply_retry_policy(policy: &mut RetryPolicy, source: &Value) {
    if source.is_null() {
        return;
    }
    if let Some(enabled) = source.get("enabled").and_then(|v| v.as_bool()) {
        policy.enabled = enabled;
    }
    if let Some(max_attempts) = source.get("max_attempts").and_then(|v| v.as_u64()) {
        policy.max_attempts = max_attempts as usize;
    }
    if let Some(base_delay) = source.get("base_delay_ms").and_then(|v| v.as_u64()) {
        policy.base_delay_ms = base_delay;
    }
    if let Some(max_delay) = source.get("max_delay_ms").and_then(|v| v.as_u64()) {
        policy.max_delay_ms = max_delay;
    }
    if let Some(jitter) = source.get("jitter").and_then(|v| v.as_f64()) {
        policy.jitter = jitter;
    }
    if let Some(status_codes) = source.get("status_codes").and_then(|v| v.as_array()) {
        policy.status_codes = status_codes
            .iter()
            .filter_map(|v| v.as_u64())
            .map(|v| v as u16)
            .collect();
    }
    if let Some(methods) = source.get("methods").and_then(|v| v.as_array()) {
        policy.methods = Some(
            methods
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
        );
    }
    if let Some(retry_on_network_error) = source
        .get("retry_on_network_error")
        .and_then(|v| v.as_bool())
    {
        policy.retry_on_network_error = retry_on_network_error;
    }
    if let Some(respect_retry_after) = source.get("respect_retry_after").and_then(|v| v.as_bool()) {
        policy.respect_retry_after = respect_retry_after;
    }
}

fn apply_cache_policy(policy: &mut CachePolicy, source: &Value) {
    if source.is_null() {
        return;
    }
    if let Some(enabled) = source.get("enabled").and_then(|v| v.as_bool()) {
        policy.enabled = enabled;
    }
    if let Some(ttl_ms) = source.get("ttl_ms").and_then(|v| v.as_u64()) {
        policy.ttl_ms = Some(ttl_ms);
    }
    if let Some(cache_errors) = source.get("cache_errors").and_then(|v| v.as_bool()) {
        policy.cache_errors = cache_errors;
    }
    if let Some(key) = source.get("key").and_then(|v| v.as_str()) {
        policy.key = Some(key.to_string());
    }
    if source.is_boolean() {
        policy.enabled = source.as_bool().unwrap_or(false);
    }
}

fn merge_action(args: &Value, action: &str, method: &str) -> Value {
    let mut clone = args.clone();
    if let Value::Object(map) = &mut clone {
        map.insert("action".to_string(), Value::String(action.to_string()));
        map.insert("method".to_string(), Value::String(method.to_string()));
    }
    clone
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for ApiManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.handle_action(args).await
    }
}
