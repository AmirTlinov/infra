use crate::errors::ToolError;
use crate::services::capability::CapabilityService;
use crate::services::context::ContextService;
use crate::services::logger::Logger;
use crate::services::validation::Validation;
use crate::utils::listing::ListFilters;
use crate::utils::tool_errors::unknown_action_error;
use crate::utils::when_matcher::matches_when;
use serde_json::Value;
use std::sync::Arc;

const CAPABILITY_ACTIONS: &[&str] = &[
    "list", "get", "set", "delete", "resolve", "suggest", "graph", "stats",
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
                }))
            }
            "get" => {
                let name = self.validation.ensure_string(
                    args.get("name").unwrap_or(&Value::Null),
                    "Capability name",
                    true,
                )?;
                let capability = self.capability_service.get_capability(&name)?;
                Ok(serde_json::json!({"success": true, "capability": capability}))
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
                let intent = self.validation.ensure_string(
                    args.get("intent").unwrap_or(&Value::Null),
                    "Intent type",
                    true,
                )?;
                let candidates = self.capability_service.find_all_by_intent(&intent)?;
                if candidates.is_empty() {
                    return Err(ToolError::not_found(format!("Capability for intent '{}' not found", intent))
                        .with_hint("Create a capability for this intent, or run capability_list to see available ones.".to_string())
                        .with_details(serde_json::json!({"intent_type": intent})));
                }
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
                let mut matched = Vec::new();
                for candidate in candidates {
                    if matches_when(candidate.get("when").unwrap_or(&Value::Null), &context) {
                        matched.push(candidate);
                    }
                }
                if matched.is_empty() {
                    return Err(ToolError::not_found(format!(
                        "No capability matched when clause for intent '{}'",
                        intent
                    ))
                    .with_hint(
                        "Adjust capability.when or provide more context for matching.".to_string(),
                    )
                    .with_details(serde_json::json!({"intent_type": intent})));
                }
                matched.sort_by(|a, b| {
                    let a_direct = a.get("name").and_then(|v| v.as_str()) == Some(intent.as_str());
                    let b_direct = b.get("name").and_then(|v| v.as_str()) == Some(intent.as_str());
                    match (a_direct, b_direct) {
                        (true, false) => std::cmp::Ordering::Less,
                        (false, true) => std::cmp::Ordering::Greater,
                        _ => a
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or("")),
                    }
                });
                Ok(
                    serde_json::json!({"success": true, "capability": matched[0].clone(), "context": context_result.and_then(|v| v.get("context").cloned())}),
                )
            }
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
