use crate::errors::ToolError;
use crate::managers::ssh::{ensure_remote_dir, SshManager};
use crate::services::logger::Logger;
use crate::services::profile::ProfileService;
use crate::services::project_resolver::ProjectResolver;
use crate::services::secret_ref::SecretRefResolver;
use crate::services::validation::Validation;
use crate::utils::feature_flags::is_allow_secret_export_enabled;
use crate::utils::stdin::{apply_stdin_source, resolve_stdin_source};
use crate::utils::tool_errors::unknown_action_error;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use ssh2::{FileStat, OpenFlags, OpenType};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

const ENV_PROFILE_TYPE: &str = "env";
const ENV_ACTIONS: &[&str] = &[
    "profile_upsert",
    "profile_get",
    "profile_list",
    "profile_delete",
    "write_remote",
    "run_remote",
];

static ENV_KEY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap());

#[derive(Clone, Default)]
struct ProjectDefaults {
    project_name: Option<String>,
    target_name: Option<String>,
    env_profile: Option<String>,
    ssh_profile: Option<String>,
    cwd: Option<String>,
    env_path: Option<String>,
}

#[derive(Clone)]
struct EnvBundle {
    variables: serde_json::Map<String, Value>,
    variable_keys: Vec<String>,
    secret_keys: Vec<String>,
}

fn normalize_string_map(
    value: Option<&Value>,
    label: &str,
    allow_null: bool,
) -> Result<Option<serde_json::Map<String, Value>>, ToolError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(Some(serde_json::Map::new()));
    }
    let obj = value
        .as_object()
        .ok_or_else(|| ToolError::invalid_params(format!("{} must be an object", label)))?;
    let mut out = serde_json::Map::new();
    for (key, raw) in obj {
        if key.trim().is_empty() {
            continue;
        }
        if raw.is_null() {
            if allow_null {
                out.insert(key.to_string(), Value::Null);
            }
            continue;
        }
        let rendered = raw
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| raw.to_string());
        out.insert(key.to_string(), Value::String(rendered));
    }
    Ok(Some(out))
}

fn normalize_env_key(key: &str) -> Result<String, ToolError> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(ToolError::invalid_params(
            "env var key must be a non-empty string",
        ));
    }
    if !ENV_KEY_RE.is_match(trimmed) {
        return Err(ToolError::invalid_params(format!(
            "Invalid env var key: {}",
            trimmed
        )));
    }
    Ok(trimmed.to_string())
}

fn escape_env_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\r' => escaped.push_str("\\r"),
            '\n' => escaped.push_str("\\n"),
            '"' => escaped.push_str("\\\""),
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

fn render_dotenv(vars: &BTreeMap<String, String>) -> String {
    if vars.is_empty() {
        return "\n".to_string();
    }
    let mut lines = String::new();
    for (idx, (key, value)) in vars.iter().enumerate() {
        if idx > 0 {
            lines.push('\n');
        }
        lines.push_str(key);
        lines.push('=');
        lines.push_str(&escape_env_value(value));
    }
    lines.push('\n');
    lines
}

fn random_token() -> String {
    let bytes: [u8; 6] = rand::random();
    hex::encode(bytes)
}

#[derive(Clone)]
pub struct EnvManager {
    logger: Logger,
    validation: Validation,
    profile_service: Arc<ProfileService>,
    ssh_manager: Arc<SshManager>,
    project_resolver: Option<Arc<ProjectResolver>>,
    secret_ref_resolver: Option<Arc<SecretRefResolver>>,
}

impl EnvManager {
    pub fn new(
        logger: Logger,
        validation: Validation,
        profile_service: Arc<ProfileService>,
        ssh_manager: Arc<SshManager>,
        project_resolver: Option<Arc<ProjectResolver>>,
        secret_ref_resolver: Option<Arc<SecretRefResolver>>,
    ) -> Self {
        Self {
            logger: logger.child("env"),
            validation,
            profile_service,
            ssh_manager,
            project_resolver,
            secret_ref_resolver,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "profile_upsert" => self.profile_upsert(&args).await,
            "profile_get" => self.profile_get(&args),
            "profile_list" => {
                let profiles = self.profile_service.list_profiles(Some(ENV_PROFILE_TYPE))?;
                Ok(serde_json::json!({"success": true, "profiles": profiles}))
            }
            "profile_delete" => {
                let name = self.validation.ensure_string(
                    args.get("profile_name").unwrap_or(&Value::Null),
                    "profile_name",
                    true,
                )?;
                let _ = self.profile_service.delete_profile(&name)?;
                Ok(serde_json::json!({"success": true, "profile": name}))
            }
            "write_remote" => self.write_remote(&args).await,
            "run_remote" => self.run_remote(&args).await,
            _ => Err(unknown_action_error("env", action, ENV_ACTIONS)),
        }
    }

    async fn resolve_profiles_from_project(&self, args: &Value) -> ProjectDefaults {
        let mut defaults = ProjectDefaults::default();
        let Some(resolver) = &self.project_resolver else {
            return defaults;
        };
        let Ok(ctx) = resolver.resolve_context(args).await else {
            return defaults;
        };
        let Some(ctx) = ctx else {
            return defaults;
        };

        defaults.project_name = ctx
            .get("projectName")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        defaults.target_name = ctx
            .get("targetName")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(target) = ctx.get("target").and_then(|v| v.as_object()) {
            defaults.env_profile = target
                .get("env_profile")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            defaults.ssh_profile = target
                .get("ssh_profile")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            defaults.cwd = target
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            defaults.env_path = target
                .get("env_path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
        defaults
    }

    fn resolve_env_profile_name(
        &self,
        args: &Value,
        defaults: &ProjectDefaults,
    ) -> Result<String, ToolError> {
        if let Some(name) = args.get("profile_name").and_then(|v| v.as_str()) {
            return self.validation.ensure_identifier(name, "profile_name");
        }
        if let Some(name) = args.get("env_profile").and_then(|v| v.as_str()) {
            return self.validation.ensure_identifier(name, "env_profile");
        }
        if let Some(name) = defaults.env_profile.as_deref() {
            return self.validation.ensure_identifier(name, "env_profile");
        }

        let profiles = self.profile_service.list_profiles(Some(ENV_PROFILE_TYPE))?;
        let arr = profiles.as_array().cloned().unwrap_or_default();
        if arr.len() == 1 {
            if let Some(name) = arr[0].get("name").and_then(|v| v.as_str()) {
                return Ok(name.to_string());
            }
        }
        if arr.is_empty() {
            return Err(ToolError::invalid_params("env profile is required (no env profiles exist)")
                .with_hint("Create an env profile first (env.profile_upsert), or pass args.profile_name explicitly.".to_string()));
        }
        Err(
            ToolError::invalid_params("env profile is required when multiple env profiles exist")
                .with_hint("Pass args.profile_name explicitly.".to_string())
                .with_details(serde_json::json!({
                    "known_profiles": arr.iter().filter_map(|v| v.get("name")).collect::<Vec<_>>()
                })),
        )
    }

    fn resolve_ssh_profile_name(
        &self,
        args: &Value,
        defaults: &ProjectDefaults,
    ) -> Result<String, ToolError> {
        if let Some(name) = args.get("ssh_profile_name").and_then(|v| v.as_str()) {
            return self.validation.ensure_identifier(name, "ssh_profile_name");
        }
        if let Some(name) = args.get("ssh_profile").and_then(|v| v.as_str()) {
            return self.validation.ensure_identifier(name, "ssh_profile");
        }
        if let Some(name) = defaults.ssh_profile.as_deref() {
            return self.validation.ensure_identifier(name, "ssh_profile");
        }
        Err(ToolError::invalid_params("ssh_profile_name is required (or configure project target.ssh_profile)")
            .with_hint("Pass args.ssh_profile_name explicitly, or set target.ssh_profile in the active project.".to_string()))
    }

    fn allow_secret_export() -> bool {
        is_allow_secret_export_enabled()
    }

    async fn load_env_bundle(&self, env_profile_name: &str) -> Result<EnvBundle, ToolError> {
        let profile = self
            .profile_service
            .get_profile(env_profile_name, Some(ENV_PROFILE_TYPE))?;
        let mut legacy_vars = profile
            .get("data")
            .and_then(|v| v.get("variables"))
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let secrets = profile
            .get("secrets")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();

        for (key, value) in secrets.iter() {
            legacy_vars.insert(key.clone(), value.clone());
        }
        let mut variable_keys: Vec<String> = legacy_vars.keys().cloned().collect();
        variable_keys.sort();
        let mut secret_keys: Vec<String> = secrets.keys().cloned().collect();
        secret_keys.sort();
        Ok(EnvBundle {
            variables: legacy_vars,
            variable_keys,
            secret_keys,
        })
    }

    async fn resolve_env_variables(
        &self,
        vars: serde_json::Map<String, Value>,
        args: &Value,
    ) -> Result<serde_json::Map<String, Value>, ToolError> {
        let input = Value::Object(vars);
        let resolved = if let Some(resolver) = &self.secret_ref_resolver {
            resolver.resolve_deep(&input, args).await?
        } else {
            input
        };
        resolved
            .as_object()
            .cloned()
            .ok_or_else(|| ToolError::internal("Resolved env variables are not an object"))
    }

    async fn profile_upsert(&self, args: &Value) -> Result<Value, ToolError> {
        let name = self.validation.ensure_string(
            args.get("profile_name").unwrap_or(&Value::Null),
            "profile_name",
            true,
        )?;
        let description = args
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let mut secrets_value: Option<Value> = None;
        if args.get("secrets").map(|v| v.is_null()).unwrap_or(false) {
            secrets_value = Some(Value::Null);
        } else if args.get("env").is_some()
            || args.get("variables").is_some()
            || args.get("secrets").is_some()
        {
            let mut merged = serde_json::Map::new();
            if let Some(map) = normalize_string_map(args.get("env"), "env", true)? {
                merged.extend(map);
            }
            if let Some(map) = normalize_string_map(args.get("variables"), "variables", true)? {
                merged.extend(map);
            }
            if let Some(map) = normalize_string_map(args.get("secrets"), "secrets", true)? {
                merged.extend(map);
            }
            secrets_value = Some(Value::Object(merged));
        }

        let mut data = serde_json::Map::new();
        if let Some(description) = description {
            data.insert("description".to_string(), Value::String(description));
        }

        let mut payload = serde_json::json!({
            "type": ENV_PROFILE_TYPE,
            "data": Value::Object(data),
        });
        if let Some(secrets) = secrets_value {
            if let Value::Object(map) = &mut payload {
                map.insert("secrets".to_string(), secrets);
            }
        }

        let _ = self.profile_service.set_profile(&name, &payload)?;
        let stored = self
            .profile_service
            .get_profile(&name, Some(ENV_PROFILE_TYPE))?;
        let keys = stored
            .get("secrets")
            .and_then(|v| v.as_object())
            .map(|map| {
                let mut out: Vec<String> = map.keys().cloned().collect();
                out.sort();
                out
            })
            .unwrap_or_default();

        Ok(serde_json::json!({
            "success": true,
            "profile": {
                "name": name,
                "type": ENV_PROFILE_TYPE,
                "description": stored.get("data").and_then(|v| v.get("description")).cloned().unwrap_or(Value::Null),
                "keys": keys,
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
            .get_profile(&name, Some(ENV_PROFILE_TYPE))?;

        let legacy_vars = profile
            .get("data")
            .and_then(|v| v.get("variables"))
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let secret_vars = profile
            .get("secrets")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let mut keys: Vec<String> = legacy_vars
            .keys()
            .chain(secret_vars.keys())
            .cloned()
            .collect();
        keys.sort();
        keys.dedup();

        let include_secrets = args
            .get("include_secrets")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if include_secrets && Self::allow_secret_export() {
            return Ok(serde_json::json!({"success": true, "profile": profile}));
        }

        let mut data = profile
            .get("data")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        if let Value::Object(map) = &mut data {
            map.insert(
                "variables".to_string(),
                Value::Array(keys.iter().cloned().map(Value::String).collect()),
            );
        }

        Ok(serde_json::json!({
            "success": true,
            "profile": {
                "name": profile.get("name").cloned().unwrap_or(Value::String(name)),
                "type": profile.get("type").cloned().unwrap_or(Value::Null),
                "data": data,
                "secrets": Value::Array(keys.iter().cloned().map(Value::String).collect()),
                "secrets_redacted": true,
            }
        }))
    }

    async fn write_remote(&self, args: &Value) -> Result<Value, ToolError> {
        let defaults = self.resolve_profiles_from_project(args).await;
        let env_profile_name = self.resolve_env_profile_name(args, &defaults)?;
        let ssh_profile_name = self.resolve_ssh_profile_name(args, &defaults)?;

        let mut remote_path = args
            .get("remote_path")
            .and_then(|v| v.as_str())
            .map(|s| {
                self.validation
                    .ensure_string(&Value::String(s.to_string()), "remote_path", false)
            })
            .transpose()?;

        let mode = args
            .get("mode")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
            })
            .unwrap_or(0o600)
            .max(0) as u32;
        let mkdirs = args
            .get("mkdirs")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let overwrite = args
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let keep_backup = args
            .get("backup")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if remote_path.is_none() {
            if let Some(env_path) = defaults.env_path.as_ref() {
                remote_path = Some(self.validation.ensure_string(
                    &Value::String(env_path.clone()),
                    "remote_path",
                    false,
                )?);
            } else if let Some(cwd) = defaults.cwd.as_ref() {
                let cwd =
                    self.validation
                        .ensure_string(&Value::String(cwd.clone()), "cwd", false)?;
                let joined = Path::new(&cwd).join(".env");
                remote_path = Some(joined.to_string_lossy().to_string());
            } else {
                return Err(ToolError::invalid_params("remote_path is required (or configure project target.env_path / target.cwd)")
                    .with_hint("Pass args.remote_path explicitly, or set target.env_path / target.cwd in the project target.".to_string()));
            }
        }
        let remote_path = remote_path.unwrap();

        let bundle = self.load_env_bundle(&env_profile_name).await?;
        let resolved_vars = self
            .resolve_env_variables(bundle.variables.clone(), args)
            .await?;
        let mut ordered: BTreeMap<String, String> = BTreeMap::new();
        for (key, value) in resolved_vars {
            let normalized = normalize_env_key(&key)?;
            let rendered = value
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| value.to_string());
            ordered.insert(normalized, rendered);
        }
        let content = render_dotenv(&ordered);

        let remote_clone = remote_path.clone();
        let content_clone = content.clone();
        let kept_backup = self
            .ssh_manager
            .with_sftp(
                &serde_json::json!({"profile_name": ssh_profile_name}),
                move |sftp| {
                    if mkdirs {
                        ensure_remote_dir(sftp, &remote_clone)?;
                    }
                    let exists = match sftp.stat(Path::new(&remote_clone)) {
                        Ok(_) => true,
                        Err(err) => {
                            let io_err: std::io::Error = err.into();
                            if io_err.kind() == std::io::ErrorKind::NotFound {
                                false
                            } else {
                                return Err(ToolError::internal(format!(
                                    "Failed to stat remote path: {}",
                                    io_err
                                )));
                            }
                        }
                    };

                    if exists && !overwrite {
                        return Err(ToolError::conflict(format!(
                            "Remote path already exists: {}",
                            remote_clone
                        ))
                        .with_hint(
                            "Set overwrite=true (optionally backup=true) to replace it."
                                .to_string(),
                        )
                        .with_details(serde_json::json!({"remote_path": remote_clone})));
                    }

                    let tmp_path = format!(
                        "{}.tmp-{}-{}-{}",
                        remote_clone,
                        std::process::id(),
                        chrono::Utc::now().timestamp_millis(),
                        random_token()
                    );
                    let backup_path = if exists {
                        Some(format!(
                            "{}.bak-{}-{}",
                            remote_clone,
                            chrono::Utc::now().timestamp_millis(),
                            random_token()
                        ))
                    } else {
                        None
                    };

                    let mut moved_to_backup = false;
                    let mut keep_backup_path: Option<String> = None;

                    let attempt = (|| -> Result<(), ToolError> {
                        {
                            let mut remote_file = sftp
                                .open_mode(
                                    Path::new(&tmp_path),
                                    OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
                                    mode as i32,
                                    OpenType::File,
                                )
                                .map_err(|err| ToolError::internal(err.to_string()))?;
                            remote_file
                                .write_all(content_clone.as_bytes())
                                .map_err(|err| ToolError::internal(err.to_string()))?;
                        }

                        let _ = sftp.setstat(
                            Path::new(&tmp_path),
                            FileStat {
                                size: None,
                                uid: None,
                                gid: None,
                                perm: Some(mode),
                                atime: None,
                                mtime: None,
                            },
                        );

                        if exists && overwrite {
                            if let Some(backup_path) = backup_path.as_ref() {
                                sftp.rename(Path::new(&remote_clone), Path::new(backup_path), None)
                                    .map_err(|err| ToolError::internal(err.to_string()))?;
                                moved_to_backup = true;
                            }
                        }

                        sftp.rename(Path::new(&tmp_path), Path::new(&remote_clone), None)
                            .map_err(|err| ToolError::internal(err.to_string()))?;

                        let _ = sftp.setstat(
                            Path::new(&remote_clone),
                            FileStat {
                                size: None,
                                uid: None,
                                gid: None,
                                perm: Some(mode),
                                atime: None,
                                mtime: None,
                            },
                        );

                        if moved_to_backup {
                            if let Some(backup_path) = backup_path.as_ref() {
                                if keep_backup {
                                    keep_backup_path = Some(backup_path.clone());
                                } else {
                                    let _ = sftp.unlink(Path::new(backup_path));
                                }
                            }
                        }
                        Ok(())
                    })();

                    if let Err(err) = attempt {
                        let _ = sftp.unlink(Path::new(&tmp_path));
                        if moved_to_backup {
                            if let Some(backup_path) = backup_path.as_ref() {
                                let _ = sftp.rename(
                                    Path::new(backup_path),
                                    Path::new(&remote_clone),
                                    None,
                                );
                            }
                        }
                        return Err(err);
                    }

                    Ok(keep_backup_path)
                },
            )
            .await?;

        let mut response = serde_json::json!({
            "success": true,
            "ssh_profile_name": ssh_profile_name,
            "env_profile_name": env_profile_name,
            "remote_path": remote_path,
            "overwrite": overwrite,
            "variables": {
                "count": bundle.variable_keys.len(),
                "keys": bundle.variable_keys,
            },
            "secrets": {
                "count": bundle.secret_keys.len(),
                "keys": bundle.secret_keys,
            }
        });
        if let Some(backup_path) = kept_backup {
            if let Value::Object(map) = &mut response {
                map.insert("backup_path".to_string(), Value::String(backup_path));
            }
        }
        Ok(response)
    }

    async fn run_remote(&self, args: &Value) -> Result<Value, ToolError> {
        let defaults = self.resolve_profiles_from_project(args).await;
        let env_profile_name = self.resolve_env_profile_name(args, &defaults)?;
        let ssh_profile_name = self.resolve_ssh_profile_name(args, &defaults)?;
        let command = self.validation.ensure_string(
            args.get("command").unwrap_or(&Value::Null),
            "command",
            false,
        )?;

        let cwd = if let Some(cwd) = args.get("cwd").and_then(|v| v.as_str()) {
            Some(cwd.to_string())
        } else {
            defaults.cwd.clone()
        };

        let bundle = self.load_env_bundle(&env_profile_name).await?;
        let resolved_vars = self
            .resolve_env_variables(bundle.variables.clone(), args)
            .await?;
        let mut env = serde_json::Map::new();
        for (key, value) in resolved_vars {
            let normalized = normalize_env_key(&key)?;
            let rendered = value
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| value.to_string());
            env.insert(normalized, Value::String(rendered));
        }

        let mut exec_args = serde_json::json!({
            "action": "exec",
            "profile_name": ssh_profile_name,
            "command": command,
            "env": Value::Object(env),
        });
        if let Some(cwd) = cwd {
            if let Value::Object(map) = &mut exec_args {
                map.insert("cwd".to_string(), Value::String(cwd));
            }
        }
        if let Some(source) = resolve_stdin_source(args)? {
            if let Value::Object(map) = &mut exec_args {
                apply_stdin_source(map, &source);
            }
        }
        if let Some(timeout_ms) = args.get("timeout_ms") {
            if let Value::Object(map) = &mut exec_args {
                map.insert("timeout_ms".to_string(), timeout_ms.clone());
            }
        }
        if let Some(stdin_eof) = args.get("stdin_eof") {
            if let Value::Object(map) = &mut exec_args {
                map.insert("stdin_eof".to_string(), stdin_eof.clone());
            }
        }
        if let Some(pty) = args.get("pty") {
            if let Value::Object(map) = &mut exec_args {
                map.insert("pty".to_string(), pty.clone());
            }
        }

        let result = self.ssh_manager.handle_action(exec_args).await?;

        Ok(serde_json::json!({
            "success": result.get("exitCode").and_then(|v| v.as_i64()).unwrap_or(-1) == 0,
            "ssh_profile_name": ssh_profile_name,
            "env_profile_name": env_profile_name,
            "variables": {
                "count": bundle.variable_keys.len(),
                "keys": bundle.variable_keys,
            },
            "command": result.get("command").cloned().unwrap_or(Value::Null),
            "stdout": result.get("stdout").cloned().unwrap_or(Value::Null),
            "stderr": result.get("stderr").cloned().unwrap_or(Value::Null),
            "exitCode": result.get("exitCode").cloned().unwrap_or(Value::Null),
            "signal": result.get("signal").cloned().unwrap_or(Value::Null),
            "timedOut": result.get("timedOut").cloned().unwrap_or(Value::Null),
            "duration_ms": result.get("duration_ms").cloned().unwrap_or(Value::Null),
        }))
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for EnvManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
