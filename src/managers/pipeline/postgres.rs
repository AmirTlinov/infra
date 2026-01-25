use super::Trace;
use crate::errors::ToolError;
use crate::managers::api::{map_reqwest_error, RequestConfig};
use bytes::Bytes;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader, DuplexStream};

impl super::PipelineManager {
    pub(super) fn build_export_args(&self, args: &Value) -> Value {
        let mut out = args
            .get("postgres")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let fields = [
            "format",
            "batch_size",
            "limit",
            "offset",
            "csv_header",
            "csv_delimiter",
            "columns",
            "columns_sql",
            "order_by",
            "order_by_sql",
            "filters",
            "where_sql",
            "where_params",
            "timeout_ms",
        ];
        for key in fields {
            if let Some(val) = args.get(key) {
                if !val.is_null() {
                    out.insert(key.to_string(), val.clone());
                }
            }
        }
        Value::Object(out)
    }

    pub(super) async fn ingest_stream(
        &self,
        reader: &mut DuplexStream,
        postgres_cfg: &Value,
        format: Option<&Value>,
        batch_size: Option<&Value>,
        max_rows: Option<&Value>,
        csv_header: Option<&Value>,
        csv_delimiter: Option<&Value>,
    ) -> Result<Value, ToolError> {
        if !postgres_cfg.is_object() {
            return Err(ToolError::invalid_params("postgres config is required"));
        }

        let format = format
            .and_then(|v| v.as_str())
            .unwrap_or("jsonl")
            .trim()
            .to_lowercase();
        if format != "jsonl" && format != "csv" {
            return Err(ToolError::invalid_params("format must be jsonl or csv"));
        }

        let batch_size = super::util::read_positive_int(batch_size).unwrap_or(500) as usize;
        let max_rows = super::util::read_positive_int(max_rows).map(|v| v as usize);

        let mut columns: Option<Vec<String>> = postgres_cfg
            .get("columns")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|entry| entry.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|arr| !arr.is_empty());

        let use_header = csv_header
            .and_then(|v| v.as_bool())
            .unwrap_or(columns.is_none());

        let delimiter = csv_delimiter
            .and_then(|v| v.as_str())
            .unwrap_or(",")
            .to_string();

        let mut rows: Vec<Value> = Vec::with_capacity(batch_size);
        let mut inserted = 0usize;

        let mut lines = BufReader::new(reader).lines();
        while let Some(line) = lines.next_line().await? {
            if max_rows.is_some() && inserted + rows.len() >= max_rows.unwrap() {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if format == "jsonl" {
                let parsed: Value = serde_json::from_str(trimmed)
                    .map_err(|_| ToolError::invalid_params("jsonl line must be valid JSON"))?;
                let is_object = parsed.is_object() && !parsed.is_array();
                if !is_object {
                    return Err(ToolError::invalid_params("jsonl line must be an object"));
                }
                rows.push(parsed);
            } else {
                let values = parse_csv_line(trimmed, &delimiter);
                if use_header && columns.is_none() {
                    columns = Some(
                        values
                            .iter()
                            .map(|entry| entry.trim().to_string())
                            .collect(),
                    );
                    continue;
                }
                let Some(cols) = columns.as_ref() else {
                    return Err(ToolError::invalid_params("csv columns are required"));
                };
                let mut row = serde_json::Map::new();
                for (idx, col) in cols.iter().enumerate() {
                    row.insert(
                        col.clone(),
                        values
                            .get(idx)
                            .map(|v| Value::String(v.clone()))
                            .unwrap_or(Value::Null),
                    );
                }
                rows.push(Value::Object(row));
            }

            if rows.len() >= batch_size {
                inserted += self
                    .flush_rows(postgres_cfg, &rows, columns.as_ref())
                    .await?;
                rows.clear();
            }
        }

        if !rows.is_empty() {
            inserted += self
                .flush_rows(postgres_cfg, &rows, columns.as_ref())
                .await?;
        }

        Ok(serde_json::json!({"inserted": inserted}))
    }

    async fn flush_rows(
        &self,
        postgres_cfg: &Value,
        rows: &[Value],
        columns: Option<&Vec<String>>,
    ) -> Result<usize, ToolError> {
        let mut args = postgres_cfg.as_object().cloned().unwrap_or_default();
        args.insert(
            "action".to_string(),
            Value::String("insert_bulk".to_string()),
        );
        args.insert("rows".to_string(), Value::Array(rows.to_vec()));
        if let Some(cols) = columns {
            args.insert(
                "columns".to_string(),
                Value::Array(cols.iter().cloned().map(Value::String).collect()),
            );
        }
        let result = self
            .postgres_manager
            .handle_action(Value::Object(args))
            .await?;
        Ok(result
            .get("inserted")
            .and_then(|v| v.as_u64())
            .unwrap_or(rows.len() as u64) as usize)
    }

    pub(super) async fn upload_postgres_to_http(
        &self,
        hydrated: &Value,
        trace: &Trace,
    ) -> Result<Value, ToolError> {
        let http_cfg = hydrated.get("http").unwrap_or(&Value::Null);
        if !http_cfg.is_object() {
            return Err(ToolError::invalid_params("http config is required"));
        }

        let mut http_args = http_cfg.as_object().cloned().unwrap_or_default();
        http_args
            .entry("method".to_string())
            .or_insert_with(|| Value::String("POST".to_string()));

        let export_args = self.build_export_args(hydrated);
        let format = export_args
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("csv")
            .trim()
            .to_lowercase();

        let mut headers = self.validation.ensure_headers(http_args.get("headers"))?;
        if !has_header(&headers, "content-type") {
            headers.insert(
                "Content-Type".to_string(),
                Value::String(
                    if format == "jsonl" {
                        "application/jsonl"
                    } else {
                        "text/csv"
                    }
                    .to_string(),
                ),
            );
        }
        http_args.insert("headers".to_string(), Value::Object(headers));

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

        self.audit_stage(
            "postgres_export",
            trace,
            serde_json::json!({"table": export_args.get("table"), "schema": export_args.get("schema"), "format": format}),
            None,
        );

        let mut attempt = 0usize;
        let mut last_err: Option<ToolError> = None;

        while attempt < max_attempts {
            attempt += 1;
            match self
                .post_http_with_fresh_export(&config, &http_value, &export_args)
                .await
            {
                Ok((http_result, summary)) => {
                    if !self.api_manager.should_retry_response(&summary, &policy)
                        || attempt >= max_attempts
                    {
                        let mut out = http_result;
                        if let Some(obj) = out.as_object_mut() {
                            obj.insert(
                                "attempts".to_string(),
                                Value::Number((attempt as u64).into()),
                            );
                            obj.insert(
                                "retries".to_string(),
                                Value::Number(((attempt.saturating_sub(1)) as u64).into()),
                            );
                        }
                        self.audit_stage(
                            "http_upload",
                            trace,
                            serde_json::json!({"url": config.url, "status": summary.get("status")}),
                            None,
                        );
                        return Ok(out);
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

    async fn post_http_with_fresh_export(
        &self,
        config: &RequestConfig,
        http_value: &Value,
        export_args: &Value,
    ) -> Result<(Value, Value), ToolError> {
        let export = self.postgres_manager.export_stream(export_args);
        let body = async_read_to_body(export.reader);

        let client = self
            .api_manager
            .get_client(config.follow_redirects, config.insecure_ok)?;
        let mut req = client.request(config.method.clone(), config.url.clone());
        req = req.headers(config.headers.clone()).body(body);
        if let Some(timeout_ms) = config.timeout_ms {
            req = req.timeout(std::time::Duration::from_millis(timeout_ms));
        }

        let response = req.send().await.map_err(map_reqwest_error)?;
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
        let response_text = response.text().await.unwrap_or_default();

        let export_result = export
            .completion
            .await
            .map_err(|_| ToolError::internal("Postgres export task failed"))??;

        let summary = serde_json::json!({
            "status": status,
            "headers": Value::Object(headers_snapshot.clone()),
        });

        let out = serde_json::json!({
            "success": (200..300).contains(&status),
            "flow": "postgres_to_http",
            "postgres": {
                "rows_written": export_result.get("rows_written").cloned().unwrap_or(Value::Null),
                "format": export_result.get("format").cloned().unwrap_or(Value::Null),
                "table": export_result.get("table").cloned().unwrap_or(Value::Null),
                "schema": export_result.get("schema").cloned().unwrap_or(Value::Null),
                "duration_ms": export_result.get("duration_ms").cloned().unwrap_or(Value::Null),
            },
            "http": {
                "url": config.url.clone(),
                "method": http_value.get("method").cloned().unwrap_or(Value::Null),
                "status": status,
                "headers": Value::Object(headers_snapshot),
                "response": response_text,
            }
        });

        Ok((out, summary))
    }
}

fn has_header(headers: &serde_json::Map<String, Value>, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    headers.keys().any(|k| k.to_lowercase() == needle)
}

fn parse_csv_line(line: &str, delimiter: &str) -> Vec<String> {
    let delim = delimiter.chars().next().unwrap_or(',');
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    let chars: Vec<char> = line.chars().collect();
    let mut idx = 0usize;
    while idx < chars.len() {
        let ch = chars[idx];
        if ch == '"' {
            if in_quotes && idx + 1 < chars.len() && chars[idx + 1] == '"' {
                current.push('"');
                idx += 2;
                continue;
            }
            in_quotes = !in_quotes;
            idx += 1;
            continue;
        }
        if ch == delim && !in_quotes {
            out.push(current.clone());
            current.clear();
            idx += 1;
            continue;
        }
        current.push(ch);
        idx += 1;
    }
    out.push(current);
    out
}

fn async_read_to_body(reader: DuplexStream) -> reqwest::Body {
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
