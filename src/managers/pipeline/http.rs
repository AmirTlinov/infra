use super::Trace;
use crate::constants::cache as cache_constants;
use crate::errors::ToolError;
use crate::managers::api::{map_reqwest_error, ApiProfile, RequestConfig};
use crate::utils::artifacts::{
    build_tool_call_file_ref, create_artifact_write_stream, resolve_context_root,
};
use crate::utils::redact::redact_text;
use bytes::Bytes;
use futures::StreamExt;
use serde_json::Value;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

pub(super) struct HttpCompletion {
    body_ref: Option<Value>,
    body_ref_truncated: bool,
}

impl HttpCompletion {
    pub(super) fn attach_body_ref(&self, response: Value) -> Value {
        let Some(body_ref) = self.body_ref.clone() else {
            return response;
        };
        let mut out = response;
        if let Value::Object(map) = &mut out {
            map.insert("body_ref".to_string(), body_ref);
            map.insert(
                "body_ref_truncated".to_string(),
                Value::Bool(self.body_ref_truncated),
            );
        }
        out
    }
}

pub(super) struct OpenedHttpStream {
    pub(super) reader: DuplexStream,
    pub(super) response: Value,
    pub(super) cache: Option<Value>,
    pub(super) completion: tokio::task::JoinHandle<Result<HttpCompletion, ToolError>>,
}

#[derive(Clone, Debug)]
struct CachePolicy {
    enabled: bool,
    ttl_ms: Option<u64>,
    key: Option<String>,
}

impl CachePolicy {
    fn disabled() -> Self {
        Self {
            enabled: false,
            ttl_ms: None,
            key: None,
        }
    }
}

impl super::PipelineManager {
    pub(super) async fn resolve_http_profile(
        &self,
        http_args: &Value,
    ) -> Result<(ApiProfile, Option<Value>), ToolError> {
        let profile = self
            .api_manager
            .resolve_profile(http_args.get("profile_name"), http_args)
            .await?;
        let mut auth = http_args.get("auth").cloned().or(profile.auth.clone());
        let auth_provider = http_args
            .get("auth_provider")
            .cloned()
            .or(profile.auth_provider.clone());

        if let Some(provider) = auth_provider {
            auth = self
                .api_manager
                .resolve_auth_provider(Some(provider), profile.name.as_deref(), http_args)
                .await?;
        }

        Ok((profile, auth))
    }

    pub(super) async fn open_http_stream(
        &self,
        http_args: &Value,
        cache_args: Option<&Value>,
        trace: &Trace,
    ) -> Result<OpenedHttpStream, ToolError> {
        if !http_args.is_object() {
            return Err(ToolError::invalid_params("http config is required"));
        }

        let (profile, auth) = self.resolve_http_profile(http_args).await?;
        let config =
            self.api_manager
                .build_request_config(http_args, &profile, auth.as_ref(), None)?;

        let cache_policy =
            self.normalize_cache(cache_args, http_args.get("cache"), profile.cache.as_ref());
        let cache_key = self.resolve_cache_key(http_args, &config, &cache_policy);
        let RequestConfig {
            url,
            method,
            headers,
            headers_raw: _,
            body,
            timeout_ms,
            follow_redirects,
            insecure_ok,
        } = config;

        if cache_policy.enabled {
            if let (Some(cache_service), Some(cache_key)) =
                (self.cache_service.as_ref(), cache_key.as_deref())
            {
                if let Ok(Some(cached)) = cache_service.get_file(cache_key, cache_policy.ttl_ms) {
                    if let Some(file_path) = cached.get("file_path").and_then(|v| v.as_str()) {
                        self.audit_stage(
                            "http_cache_hit",
                            trace,
                            serde_json::json!({"url": url, "cache_key": cache_key}),
                            None,
                        );
                        return self.open_file_stream(
                            PathBuf::from(file_path),
                            serde_json::json!({"url": url, "method": method.as_str()}),
                            Some(serde_json::json!({"hit": true, "key": cache_key})),
                            trace,
                            "http-body",
                        );
                    }
                }
            }
        }

        let client = self.api_manager.get_client(follow_redirects, insecure_ok)?;
        let mut req = client.request(method.clone(), url.clone());
        req = req.headers(headers.clone());
        if let Some(body) = body {
            req = req.body(body);
        }
        if let Some(timeout_ms) = timeout_ms {
            req = req.timeout(std::time::Duration::from_millis(timeout_ms));
        }

        let response = req.send().await.map_err(map_reqwest_error)?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            let preview = read_error_preview(response).await;
            let redacted = redact_text(&preview, 16 * 1024, None);
            let details = serde_json::json!({"status": status, "body": redacted});
            let err = if status == 401 || status == 403 {
                ToolError::denied(format!("HTTP source failed ({})", status))
                    .with_hint(
                        "Check auth / auth_provider configuration for the API profile.".to_string(),
                    )
                    .with_details(details)
            } else if status == 404 {
                ToolError::not_found(format!("HTTP source failed ({})", status))
                    .with_hint("Verify the URL/path is correct.".to_string())
                    .with_details(details)
            } else if status == 429 || status >= 500 {
                ToolError::retryable(format!("HTTP source failed ({})", status))
                    .with_hint("Retry later or increase timeout/retries.".to_string())
                    .with_details(details)
            } else {
                ToolError::invalid_params(format!("HTTP source failed ({})", status))
                    .with_hint(
                        "Check request parameters (headers/query/body) and retry.".to_string(),
                    )
                    .with_details(details)
            };
            self.audit_stage(
                "http_fetch",
                trace,
                serde_json::json!({"url": url, "status": status}),
                Some(&err),
            );
            return Err(err);
        }

        if cache_policy.enabled {
            if let (Some(cache_service), Some(cache_key)) =
                (self.cache_service.as_ref(), cache_key.as_deref())
            {
                let stored_path = self
                    .store_response_to_cache(
                        cache_service,
                        cache_key,
                        cache_policy.ttl_ms,
                        &url,
                        method.as_str(),
                        response,
                    )
                    .await?;
                self.audit_stage(
                    "http_cache_store",
                    trace,
                    serde_json::json!({"url": url, "cache_key": cache_key}),
                    None,
                );
                return self.open_file_stream(
                    stored_path,
                    serde_json::json!({"url": url, "method": method.as_str(), "status": status}),
                    Some(serde_json::json!({"hit": false, "key": cache_key})),
                    trace,
                    "http-body",
                );
            }
        }

        self.open_network_stream(
            response,
            serde_json::json!({"url": url, "method": method.as_str(), "status": status}),
            cache_key.map(|key| serde_json::json!({"hit": false, "key": key})),
            trace,
            "http-body",
        )
    }

    fn normalize_cache(
        &self,
        cache_config: Option<&Value>,
        request_cache: Option<&Value>,
        profile_cache: Option<&Value>,
    ) -> CachePolicy {
        if cache_config.is_none() && request_cache.is_none() && profile_cache.is_none() {
            return CachePolicy::disabled();
        }

        let mut merged = serde_json::Map::new();
        merged.insert("enabled".to_string(), Value::Bool(true));
        apply_cache_layer(&mut merged, profile_cache);
        apply_cache_layer(&mut merged, request_cache);
        apply_cache_layer(&mut merged, cache_config);

        if !merged
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
        {
            return CachePolicy::disabled();
        }

        let ttl_ms = merged
            .get("ttl_ms")
            .and_then(|v| v.as_u64())
            .or(Some(cache_constants::DEFAULT_TTL_MS));
        let key = merged
            .get("key")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        CachePolicy {
            enabled: true,
            ttl_ms,
            key,
        }
    }

    fn resolve_cache_key(
        &self,
        http_args: &Value,
        config: &RequestConfig,
        policy: &CachePolicy,
    ) -> Option<String> {
        if !policy.enabled {
            return None;
        }
        let cache_service = self.cache_service.as_ref()?;

        if let Some(key) = policy.key.as_ref() {
            return cache_service.normalize_key(Some(&Value::String(key.clone())));
        }

        let input = serde_json::json!({
            "url": config.url.clone(),
            "method": config.method.as_str(),
            "headers": config.headers_raw.clone(),
            "body": http_args.get("body")
                .or_else(|| http_args.get("data"))
                .or_else(|| http_args.get("form"))
                .or_else(|| http_args.get("body_base64"))
                .cloned()
                .unwrap_or(Value::Null),
        });
        Some(cache_service.build_key(&input))
    }

    async fn store_response_to_cache(
        &self,
        cache_service: &crate::services::cache::CacheService,
        cache_key: &str,
        ttl_ms: Option<u64>,
        url: &str,
        method: &str,
        response: reqwest::Response,
    ) -> Result<PathBuf, ToolError> {
        let (_, tmp_path) = cache_service.create_file_writer(cache_key, ttl_ms, None)?;
        if let Some(parent) = tmp_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let mut file = tokio::fs::File::create(&tmp_path).await?;

        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_reqwest_error)?;
            file.write_all(&chunk).await?;
        }
        file.flush().await.ok();
        drop(file);

        let meta = Some(serde_json::json!({"url": url, "method": method}));
        cache_service.finalize_file_writer(cache_key, &tmp_path, ttl_ms, meta)?;
        cache_service.data_path(cache_key)
    }

    fn open_file_stream(
        &self,
        file_path: PathBuf,
        response_meta: Value,
        cache: Option<Value>,
        trace: &Trace,
        prefix: &str,
    ) -> Result<OpenedHttpStream, ToolError> {
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        let trace = trace.clone();
        let prefix = prefix.to_string();

        let completion = tokio::spawn(async move {
            let mut file = tokio::fs::File::open(&file_path).await?;
            let mut buf = vec![0u8; 64 * 1024];

            let mut capture = ArtifactCapture::new(trace, prefix).await;
            loop {
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                writer.write_all(&buf[..n]).await?;
                capture.capture_bytes(&buf[..n]).await;
            }
            let _ = writer.shutdown().await;

            let body_ref_truncated = capture.truncated;
            Ok(HttpCompletion {
                body_ref: capture.finalize().await,
                body_ref_truncated,
            })
        });

        Ok(OpenedHttpStream {
            reader,
            response: response_meta,
            cache,
            completion,
        })
    }

    fn open_network_stream(
        &self,
        response: reqwest::Response,
        response_meta: Value,
        cache: Option<Value>,
        trace: &Trace,
        prefix: &str,
    ) -> Result<OpenedHttpStream, ToolError> {
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        let trace = trace.clone();
        let prefix = prefix.to_string();

        let completion = tokio::spawn(async move {
            let mut capture = ArtifactCapture::new(trace, prefix).await;
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(map_reqwest_error)?;
                writer.write_all(&chunk).await?;
                capture.capture_bytes(&chunk).await;
            }
            let _ = writer.shutdown().await;

            let body_ref_truncated = capture.truncated;
            Ok(HttpCompletion {
                body_ref: capture.finalize().await,
                body_ref_truncated,
            })
        });

        Ok(OpenedHttpStream {
            reader,
            response: response_meta,
            cache,
            completion,
        })
    }

    pub(super) async fn upload_sftp_to_http(
        &self,
        http_cfg: &Value,
        sftp_cfg: &Value,
        trace: &Trace,
    ) -> Result<Value, ToolError> {
        if !http_cfg.is_object() {
            return Err(ToolError::invalid_params("http config is required"));
        }
        if !sftp_cfg.is_object() {
            return Err(ToolError::invalid_params("sftp config is required"));
        }

        let mut http_args = http_cfg.as_object().cloned().unwrap_or_default();
        http_args
            .entry("method".to_string())
            .or_insert_with(|| Value::String("PUT".to_string()));
        for key in ["body", "data", "form", "body_base64", "body_type"] {
            http_args.remove(key);
        }
        let http_value = Value::Object(http_args);
        let (profile, auth) = self.resolve_http_profile(&http_value).await?;
        let config =
            self.api_manager
                .build_request_config(&http_value, &profile, auth.as_ref(), None)?;

        let policy = self.api_manager.normalize_retry_policy(
            http_value.get("retry"),
            profile.retry.as_ref(),
            http_value.get("method"),
        );
        let max_attempts = if policy.enabled {
            policy.max_attempts.max(1)
        } else {
            1
        };

        let mut attempt = 0usize;
        let mut last_err: Option<ToolError> = None;

        while attempt < max_attempts {
            attempt += 1;

            let opened = self.open_sftp_stream(sftp_cfg).await?;
            let body = duplex_to_body(opened.reader);

            let client = self
                .api_manager
                .get_client(config.follow_redirects, config.insecure_ok)?;
            let mut req = client.request(config.method.clone(), config.url.clone());
            req = req.headers(config.headers.clone()).body(body);
            if let Some(timeout_ms) = config.timeout_ms {
                req = req.timeout(std::time::Duration::from_millis(timeout_ms));
            }

            let sent = req.send().await.map_err(map_reqwest_error);
            let read_result = opened
                .completion
                .await
                .map_err(|_| ToolError::internal("SFTP stream task failed"))?;

            read_result?;

            match sent {
                Ok(response) => {
                    let status = response.status().as_u16() as u64;
                    let headers_snapshot = response
                        .headers()
                        .iter()
                        .filter_map(|(k, v)| {
                            v.to_str()
                                .ok()
                                .map(|val| (k.to_string(), Value::String(val.to_string())))
                        })
                        .collect::<serde_json::Map<_, _>>();
                    let summary = serde_json::json!({
                        "status": status,
                        "headers": Value::Object(headers_snapshot.clone()),
                    });

                    if !self.api_manager.should_retry_response(&summary, &policy)
                        || attempt >= max_attempts
                    {
                        let response_text = response.text().await.unwrap_or_default();
                        self.audit_stage(
                            "http_upload",
                            trace,
                            serde_json::json!({"url": config.url, "status": status}),
                            None,
                        );
                        return Ok(serde_json::json!({
                            "success": (200..300).contains(&status),
                            "flow": "sftp_to_http",
                            "http": {
                                "url": config.url,
                                "method": config.method.as_str(),
                                "status": status,
                                "headers": Value::Object(headers_snapshot),
                                "response": response_text,
                            },
                            "attempts": attempt,
                            "retries": attempt.saturating_sub(1),
                        }));
                    }

                    let delay =
                        self.api_manager
                            .compute_retry_delay(attempt, &policy, Some(&summary));
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                Err(err) => {
                    last_err = Some(err.clone());
                    if !policy.retry_on_network_error || attempt >= max_attempts {
                        self.audit_stage(
                            "http_upload",
                            trace,
                            serde_json::json!({"url": config.url}),
                            Some(&err),
                        );
                        return Err(err);
                    }
                    let delay = self.api_manager.compute_retry_delay(attempt, &policy, None);
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| ToolError::retryable("HTTP upload failed after retries")))
    }
}

fn apply_cache_layer(target: &mut serde_json::Map<String, Value>, layer: Option<&Value>) {
    let Some(layer) = layer else {
        return;
    };
    if layer.is_null() {
        return;
    }
    if let Some(flag) = layer.as_bool() {
        if !flag {
            target.insert("enabled".to_string(), Value::Bool(false));
        } else {
            target.insert("enabled".to_string(), Value::Bool(true));
        }
        return;
    }
    if let Some(obj) = layer.as_object() {
        for (k, v) in obj {
            target.insert(k.clone(), v.clone());
        }
    }
}

async fn read_error_preview(response: reqwest::Response) -> String {
    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let limit = 32 * 1024usize;
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            break;
        };
        let remaining = limit.saturating_sub(buf.len());
        if remaining == 0 {
            break;
        }
        if chunk.len() <= remaining {
            buf.extend_from_slice(&chunk);
        } else {
            buf.extend_from_slice(&chunk[..remaining]);
            break;
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

struct ArtifactCapture {
    writer: Option<crate::utils::artifacts::ArtifactWriter>,
    written: usize,
    total: usize,
    limit: usize,
    truncated: bool,
}

impl ArtifactCapture {
    async fn new(trace: Trace, prefix: String) -> Self {
        let mode = super::util::resolve_stream_to_artifact_mode();
        let context_root = mode.and_then(|_| resolve_context_root());
        let Some(context_root) = context_root else {
            return Self::disabled();
        };

        let limit = match mode {
            Some(super::util::StreamToArtifactMode::Full) => usize::MAX,
            Some(super::util::StreamToArtifactMode::Capped) => {
                super::util::resolve_max_capture_bytes()
            }
            None => 0,
        };
        if limit == 0 {
            return Self::disabled();
        }

        let span_id = trace
            .parent_span_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let filename = format!("{}-{}.bin", prefix, uuid::Uuid::new_v4());
        let reference =
            match build_tool_call_file_ref(Some(&trace.trace_id), Some(&span_id), &filename) {
                Ok(r) => r,
                Err(_) => return Self::disabled(),
            };

        let writer = create_artifact_write_stream(&context_root, &reference)
            .await
            .ok();
        Self {
            writer,
            written: 0,
            total: 0,
            limit,
            truncated: false,
        }
    }

    fn disabled() -> Self {
        Self {
            writer: None,
            written: 0,
            total: 0,
            limit: 0,
            truncated: false,
        }
    }

    async fn capture_bytes(&mut self, chunk: &[u8]) {
        self.total += chunk.len();
        if self.writer.is_none() {
            if self.limit != usize::MAX {
                self.truncated = true;
            }
            return;
        }

        if self.written >= self.limit {
            if self.limit != usize::MAX {
                self.truncated = true;
            }
            return;
        }
        let remaining = self.limit - self.written;
        let slice = if chunk.len() <= remaining {
            chunk
        } else {
            &chunk[..remaining]
        };
        if slice.len() < chunk.len() {
            self.truncated = true;
        }

        let write_ok = match self.writer.as_mut() {
            Some(writer) => writer.write(slice).await.is_ok(),
            None => false,
        };

        if write_ok {
            self.written += slice.len();
            return;
        }

        if let Some(writer) = self.writer.take() {
            let _ = writer.abort().await;
        }
        self.truncated = true;
    }

    async fn finalize(mut self) -> Option<Value> {
        let writer = self.writer.take()?;
        if self.written == 0 {
            let _ = writer.abort().await;
            return None;
        }
        let finalized = writer.finalize().await.ok()?;
        Some(
            serde_json::json!({"uri": finalized.uri, "rel": finalized.rel, "bytes": finalized.bytes}),
        )
    }
}

fn duplex_to_body(reader: DuplexStream) -> reqwest::Body {
    let stream = futures::stream::try_unfold(reader, |mut reader| async move {
        let mut buf = vec![0u8; 64 * 1024];
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            Ok::<Option<(Bytes, DuplexStream)>, std::io::Error>(None)
        } else {
            Ok(Some((Bytes::copy_from_slice(&buf[..n]), reader)))
        }
    });
    reqwest::Body::wrap_stream(stream)
}
