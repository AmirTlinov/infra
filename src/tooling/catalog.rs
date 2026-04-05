use crate::errors::{ContractError, ErrorCode};
use crate::tooling::names::canonical_tool_name;
use jsonschema::JSONSchema;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

static TOOL_CONTRACT_CATALOG: Lazy<Vec<ToolDef>> = Lazy::new(|| {
    let raw = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tool_contracts.json"));
    serde_json::from_str(raw).expect("tool_contracts.json must be valid JSON")
});

static TOOL_MAP: Lazy<HashMap<String, ToolDef>> = Lazy::new(|| {
    TOOL_CONTRACT_CATALOG
        .iter()
        .cloned()
        .map(|tool| (tool.name.clone(), tool))
        .collect()
});

static TOOL_VALIDATORS: Lazy<HashMap<String, JSONSchema>> = Lazy::new(|| {
    let mut map = HashMap::new();
    for tool in TOOL_CONTRACT_CATALOG.iter() {
        if let Ok(schema) = JSONSchema::compile(&tool.input_schema) {
            map.insert(tool.name.clone(), schema);
        }
    }
    map
});

pub fn tool_contract_catalog() -> &'static [ToolDef] {
    TOOL_CONTRACT_CATALOG.as_slice()
}

pub fn tool_by_name(name: &str) -> Option<&'static ToolDef> {
    let canonical = canonical_tool_name(name);
    TOOL_MAP.get(canonical)
}

pub fn validate_tool_args(tool_name: &str, args: &Value) -> Result<(), ContractError> {
    let canonical = canonical_tool_name(tool_name);
    let Some(tool) = tool_by_name(canonical) else {
        return Ok(());
    };
    let Some(schema) = TOOL_VALIDATORS.get(&tool.name) else {
        return Ok(());
    };
    if let Err(errors) = schema.validate(args) {
        let action = args.get("action").and_then(|value| value.as_str());
        let mut lines = vec![if let Some(action) = action {
            format!("Invalid arguments for {}:{}", tool.name, action)
        } else {
            format!("Invalid arguments for {}", tool.name)
        }];
        for err in errors.take(10) {
            let instance_path = if err.instance_path.to_string().is_empty() {
                "(root)".to_string()
            } else {
                err.instance_path.to_string()
            };
            lines.push(format!("- {}: {}", instance_path, err));
        }
        return Err(ContractError::new(
            ErrorCode::InvalidParams,
            lines.join("\n"),
        ));
    }
    Ok(())
}
