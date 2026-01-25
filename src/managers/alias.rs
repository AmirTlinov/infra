use crate::errors::ToolError;
use crate::services::alias::AliasService;
use crate::services::logger::Logger;
use crate::utils::listing::ListFilters;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

const ALIAS_ACTIONS: &[&str] = &[
    "alias_upsert",
    "alias_get",
    "alias_list",
    "alias_delete",
    "alias_resolve",
];

#[derive(Clone)]
pub struct AliasManager {
    logger: Logger,
    alias_service: Arc<AliasService>,
}

impl AliasManager {
    pub fn new(logger: Logger, alias_service: Arc<AliasService>) -> Self {
        Self {
            logger: logger.child("alias"),
            alias_service,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "alias_upsert" => {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let alias = args.get("alias").cloned().unwrap_or(args.clone());
                self.alias_service.set_alias(name, &alias)
            }
            "alias_get" => {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                self.alias_service.get_alias(name)
            }
            "alias_list" => {
                let filters = ListFilters::from_args(&args);
                self.alias_service.list_aliases(&filters)
            }
            "alias_delete" => {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                self.alias_service.delete_alias(name)
            }
            "alias_resolve" => {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let resolved = self.alias_service.resolve_alias(name);
                Ok(serde_json::json!({
                    "success": true,
                    "alias": resolved.map(|value| {
                        let mut map = value.as_object().cloned().unwrap_or_default();
                        map.insert("name".to_string(), Value::String(name.to_string()));
                        Value::Object(map)
                    })
                }))
            }
            _ => Err(unknown_action_error("alias", action, ALIAS_ACTIONS)),
        }
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for AliasManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
