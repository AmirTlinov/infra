use crate::errors::ToolError;
use serde_json::Value;

impl super::PipelineManager {
    pub(super) async fn http_to_sftp(&self, args: &Value) -> Result<Value, ToolError> {
        let hydrated = self.hydrate_project_defaults(args).await?;
        let trace = self.build_trace(&hydrated);

        let http_cfg = hydrated.get("http").unwrap_or(&Value::Null);
        let cache_cfg = hydrated.get("cache");
        let mut opened = self.open_http_stream(http_cfg, cache_cfg, &trace).await?;

        self.audit_stage(
            "http_fetch",
            &trace,
            serde_json::json!({"url": opened.response.get("url"), "method": opened.response.get("method"), "cache": opened.cache.clone()}),
            None,
        );

        let sftp_cfg = hydrated.get("sftp").unwrap_or(&Value::Null);
        let sftp_result = self
            .upload_stream_to_sftp(&mut opened.reader, sftp_cfg)
            .await?;
        self.audit_stage(
            "sftp_upload",
            &trace,
            serde_json::json!({"remote_path": sftp_result.get("remote_path")}),
            None,
        );

        let completion = opened
            .completion
            .await
            .map_err(|_| ToolError::internal("HTTP stream task failed"))??;
        let http_response = completion.attach_body_ref(opened.response);

        Ok(serde_json::json!({
            "success": true,
            "flow": "http_to_sftp",
            "http": http_response,
            "sftp": sftp_result,
            "cache": opened.cache,
        }))
    }

    pub(super) async fn http_to_postgres(&self, args: &Value) -> Result<Value, ToolError> {
        let hydrated = self.hydrate_project_defaults(args).await?;
        let trace = self.build_trace(&hydrated);

        let http_cfg = hydrated.get("http").unwrap_or(&Value::Null);
        let cache_cfg = hydrated.get("cache");
        let mut opened = self.open_http_stream(http_cfg, cache_cfg, &trace).await?;

        self.audit_stage(
            "http_fetch",
            &trace,
            serde_json::json!({"url": opened.response.get("url"), "method": opened.response.get("method"), "cache": opened.cache.clone()}),
            None,
        );

        let pg_cfg = hydrated.get("postgres").unwrap_or(&Value::Null);
        let ingest = self
            .ingest_stream(
                &mut opened.reader,
                pg_cfg,
                hydrated.get("format"),
                hydrated.get("batch_size"),
                hydrated.get("max_rows"),
                hydrated.get("csv_header"),
                hydrated.get("csv_delimiter"),
            )
            .await?;

        self.audit_stage(
            "postgres_insert",
            &trace,
            serde_json::json!({"inserted": ingest.get("inserted"), "table": pg_cfg.get("table")}),
            None,
        );

        let completion = opened
            .completion
            .await
            .map_err(|_| ToolError::internal("HTTP stream task failed"))??;
        let http_response = completion.attach_body_ref(opened.response);

        Ok(serde_json::json!({
            "success": true,
            "flow": "http_to_postgres",
            "http": http_response,
            "postgres": ingest,
            "cache": opened.cache,
        }))
    }

    pub(super) async fn sftp_to_postgres(&self, args: &Value) -> Result<Value, ToolError> {
        let hydrated = self.hydrate_project_defaults(args).await?;
        let trace = self.build_trace(&hydrated);

        let sftp_cfg = hydrated.get("sftp").unwrap_or(&Value::Null);
        let mut opened = self.open_sftp_stream(sftp_cfg).await?;
        self.audit_stage(
            "sftp_download",
            &trace,
            serde_json::json!({"remote_path": sftp_cfg.get("remote_path")}),
            None,
        );

        let pg_cfg = hydrated.get("postgres").unwrap_or(&Value::Null);
        let ingest = self
            .ingest_stream(
                &mut opened.reader,
                pg_cfg,
                hydrated.get("format"),
                hydrated.get("batch_size"),
                hydrated.get("max_rows"),
                hydrated.get("csv_header"),
                hydrated.get("csv_delimiter"),
            )
            .await?;
        opened
            .completion
            .await
            .map_err(|_| ToolError::internal("SFTP stream task failed"))??;

        self.audit_stage(
            "postgres_insert",
            &trace,
            serde_json::json!({"inserted": ingest.get("inserted"), "table": pg_cfg.get("table")}),
            None,
        );

        Ok(serde_json::json!({
            "success": true,
            "flow": "sftp_to_postgres",
            "sftp": { "remote_path": sftp_cfg.get("remote_path").cloned().unwrap_or(Value::Null) },
            "postgres": ingest,
        }))
    }

    pub(super) async fn sftp_to_http(&self, args: &Value) -> Result<Value, ToolError> {
        let hydrated = self.hydrate_project_defaults(args).await?;
        let trace = self.build_trace(&hydrated);

        let http_cfg = hydrated.get("http").unwrap_or(&Value::Null);
        let sftp_cfg = hydrated.get("sftp").unwrap_or(&Value::Null);

        self.upload_sftp_to_http(http_cfg, sftp_cfg, &trace).await
    }

    pub(super) async fn postgres_to_sftp(&self, args: &Value) -> Result<Value, ToolError> {
        let hydrated = self.hydrate_project_defaults(args).await?;
        let trace = self.build_trace(&hydrated);

        let export_args = self.build_export_args(&hydrated);
        let mut export = self.postgres_manager.export_stream(&export_args);

        self.audit_stage(
            "postgres_export",
            &trace,
            serde_json::json!({"table": export_args.get("table"), "schema": export_args.get("schema"), "format": export_args.get("format")}),
            None,
        );

        let sftp_cfg = hydrated.get("sftp").unwrap_or(&Value::Null);
        let sftp_result = self
            .upload_stream_to_sftp(&mut export.reader, sftp_cfg)
            .await?;

        let export_result = export
            .completion
            .await
            .map_err(|_| ToolError::internal("Postgres export task failed"))??;

        self.audit_stage(
            "sftp_upload",
            &trace,
            serde_json::json!({"remote_path": sftp_result.get("remote_path")}),
            None,
        );

        Ok(serde_json::json!({
            "success": true,
            "flow": "postgres_to_sftp",
            "postgres": {
                "rows_written": export_result.get("rows_written").cloned().unwrap_or(Value::Null),
                "format": export_result.get("format").cloned().unwrap_or(Value::Null),
                "table": export_result.get("table").cloned().unwrap_or(Value::Null),
                "schema": export_result.get("schema").cloned().unwrap_or(Value::Null),
                "duration_ms": export_result.get("duration_ms").cloned().unwrap_or(Value::Null),
            },
            "sftp": sftp_result,
        }))
    }

    pub(super) async fn postgres_to_http(&self, args: &Value) -> Result<Value, ToolError> {
        let hydrated = self.hydrate_project_defaults(args).await?;
        let trace = self.build_trace(&hydrated);
        self.upload_postgres_to_http(&hydrated, &trace).await
    }
}
