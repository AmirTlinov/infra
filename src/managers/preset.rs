use crate::errors::ToolError;
use crate::services::logger::Logger;
use crate::services::preset::PresetService;
use crate::utils::listing::ListFilters;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

const PRESET_ACTIONS: &[&str] = &[
    "preset_upsert",
    "preset_get",
    "preset_list",
    "preset_delete",
];

#[derive(Clone)]
pub struct PresetManager {
    logger: Logger,
    preset_service: Arc<PresetService>,
}

impl PresetManager {
    pub fn new(logger: Logger, preset_service: Arc<PresetService>) -> Self {
        Self {
            logger: logger.child("preset"),
            preset_service,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "preset_upsert" => {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let preset = args.get("preset").cloned().unwrap_or(args.clone());
                self.preset_service.set_preset(name, &preset)
            }
            "preset_get" => {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                self.preset_service.get_preset(name)
            }
            "preset_list" => {
                let filters = ListFilters::from_args(&args);
                self.preset_service.list_presets(&filters)
            }
            "preset_delete" => {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                self.preset_service.delete_preset(name)
            }
            _ => Err(unknown_action_error("preset", action, PRESET_ACTIONS)),
        }
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for PresetManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
