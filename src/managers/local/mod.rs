use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::validation::Validation;
use crate::utils::feature_flags::is_unsafe_local_enabled;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;

mod exec;
mod fs;

const LOCAL_ACTIONS: &[&str] = &[
    "exec", "batch", "fs_read", "fs_write", "fs_list", "fs_stat", "fs_mkdir", "fs_rm",
];

fn read_positive_int(value: Option<&Value>) -> Option<usize> {
    let value = value?;
    if let Some(n) = value.as_i64() {
        if n > 0 {
            return Some(n as usize);
        }
    }
    if let Some(text) = value.as_str() {
        if let Ok(parsed) = text.parse::<usize>() {
            if parsed > 0 {
                return Some(parsed);
            }
        }
    }
    None
}

fn random_token() -> String {
    use rand::{distributions::Alphanumeric, Rng};
    let mut rng = rand::thread_rng();
    (0..12).map(|_| rng.sample(Alphanumeric) as char).collect()
}

#[derive(Clone)]
pub struct LocalManager {
    logger: Logger,
    validation: Validation,
    enabled: bool,
}

impl LocalManager {
    pub fn new(logger: Logger, validation: Validation, enabled: Option<bool>) -> Self {
        Self {
            logger: logger.child("local"),
            validation,
            enabled: enabled.unwrap_or_else(is_unsafe_local_enabled),
        }
    }

    fn ensure_enabled(&self) -> Result<(), ToolError> {
        if !self.enabled {
            return Err(ToolError::denied("Unsafe local tool is disabled.")
                .with_hint("Set INFRA_UNSAFE_LOCAL=1 to enable it.".to_string()));
        }
        Ok(())
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        self.ensure_enabled()?;
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "exec" => self.exec(args).await,
            "batch" => self.batch(args).await,
            "fs_read" => self.fs_read(args).await,
            "fs_write" => self.fs_write(args).await,
            "fs_list" => self.fs_list(args).await,
            "fs_stat" => self.fs_stat(args).await,
            "fs_mkdir" => self.fs_mkdir(args).await,
            "fs_rm" => self.fs_rm(args).await,
            _ => Err(unknown_action_error("local", action, LOCAL_ACTIONS)),
        }
    }

    fn normalize_env(
        &self,
        env: Option<&Value>,
    ) -> Result<Option<Vec<(String, String)>>, ToolError> {
        let Some(env) = env else {
            return Ok(None);
        };
        let obj = env
            .as_object()
            .ok_or_else(|| ToolError::invalid_params("env must be an object"))?;
        let mut out = Vec::new();
        for (key, value) in obj {
            if key.trim().is_empty() || value.is_null() {
                continue;
            }
            let rendered = value
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| value.to_string());
            out.push((key.trim().to_string(), rendered));
        }
        Ok(Some(out))
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for LocalManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
