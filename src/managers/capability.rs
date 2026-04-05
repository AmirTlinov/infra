use crate::errors::ToolError;
use crate::services::capability::CapabilityService;
use crate::services::context::ContextService;
use crate::services::logger::Logger;
use crate::services::validation::Validation;
use crate::utils::listing::ListFilters;
use crate::utils::tool_errors::unknown_action_error;
use serde_json::Value;
use std::sync::Arc;

pub(crate) const CAPABILITY_ACTIONS: &[&str] = &[
    "list", "get", "set", "delete", "resolve", "families", "suggest", "graph", "stats",
];

#[derive(Clone)]
pub struct CapabilityManager {
    logger: Logger,
    validation: Validation,
    capability_service: Arc<CapabilityService>,
    context_service: Option<Arc<ContextService>>,
}

impl CapabilityManager {
    pub fn new(
        logger: Logger,
        validation: Validation,
        capability_service: Arc<CapabilityService>,
        context_service: Option<Arc<ContextService>>,
    ) -> Self {
        Self {
            logger: logger.child("capability"),
            validation,
            capability_service,
            context_service,
        }
    }

    pub async fn handle_action(&self, args: Value) -> Result<Value, ToolError> {
        let action = args.get("action");
        match action.and_then(|v| v.as_str()).unwrap_or("") {
            "list" => {
                let filters = ListFilters::from_args(&args);
                let list = self.capability_service.list_capabilities()?;
                let items = list.as_array().cloned().unwrap_or_default();
                let result = filters.apply(
                    items,
                    &["name", "intent", "description", "runbook"],
                    Some("tags"),
                );
                Ok(serde_json::json!({
                    "success": true,
                    "capabilities": result.items,
                    "meta": filters.meta(result.total, result.items.len()),
                    "manifest": self.capability_service.manifest_metadata(),
                }))
            }
            "get" => {
                let name = self.validation.ensure_string(
                    args.get("name").unwrap_or(&Value::Null),
                    "Capability name",
                    true,
                )?;
                let capability = self.capability_service.get_capability(&name)?;
                Ok(serde_json::json!({
                    "success": true,
                    "capability": capability,
                    "manifest": self.capability_service.manifest_metadata(),
                }))
            }
            "set" => {
                let name = self.validation.ensure_string(
                    args.get("name").unwrap_or(&Value::Null),
                    "Capability name",
                    true,
                )?;
                let config = args
                    .get("capability")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                let capability = self.capability_service.set_capability(&name, &config)?;
                Ok(serde_json::json!({"success": true, "capability": capability}))
            }
            "delete" => {
                let name = self.validation.ensure_string(
                    args.get("name").unwrap_or(&Value::Null),
                    "Capability name",
                    true,
                )?;
                self.capability_service.delete_capability(&name)
            }
            "resolve" => {
                let context_result = if let Some(service) = &self.context_service {
                    service.get_context(&args).await.ok()
                } else {
                    None
                };
                let context = context_result
                    .as_ref()
                    .and_then(|v| v.get("context"))
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                let capability = if let Some(intent) = args
                    .get("intent")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    self.capability_service
                        .resolve_by_intent(intent, Some(&context))?
                } else {
                    let family = self.validation.ensure_string(
                        args.get("family").unwrap_or(&Value::Null),
                        "family",
                        true,
                    )?;
                    let verb = self.validation.ensure_string(
                        args.get("verb")
                            .or_else(|| args.get("operation"))
                            .unwrap_or(&Value::Null),
                        "verb",
                        true,
                    )?;
                    self.capability_service
                        .resolve_for_operation(&family, &verb, Some(&context))?
                };
                Ok(serde_json::json!({
                    "success": true,
                    "capability": capability,
                    "context": context_result.and_then(|v| v.get("context").cloned()),
                    "manifest": self.capability_service.manifest_metadata(),
                }))
            }
            "families" => Ok(serde_json::json!({
                "success": true,
                "families": self.capability_service.families_index()?,
                "manifest": self.capability_service.manifest_metadata(),
            })),
            "suggest" => Ok(serde_json::json!({"success": true, "suggestions": []})),
            "graph" => Ok(serde_json::json!({"success": true, "graph": []})),
            "stats" => Ok(serde_json::json!({"success": true, "stats": {}})),
            _ => Err(unknown_action_error(
                "capability",
                action,
                CAPABILITY_ACTIONS,
            )),
        }
    }
}

#[async_trait::async_trait]
impl crate::services::tool_executor::ToolHandler for CapabilityManager {
    async fn handle(&self, args: Value) -> Result<Value, ToolError> {
        self.logger.debug("handle_action", args.get("action"));
        self.handle_action(args).await
    }
}
