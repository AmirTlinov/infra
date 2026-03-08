use crate::app::App;
use crate::errors::ToolError;
use crate::mcp::catalog::{discovery_tool_catalog, is_core_discovery_tool, tool_by_name};
use crate::mcp::legend;
use crate::utils::listing::ListFilters;
use serde_json::Value;

fn as_resource(uri: &str, name: &str, description: &str) -> Value {
    serde_json::json!({
        "uri": uri,
        "name": name,
        "description": description,
        "mimeType": "application/json",
    })
}

fn json_content(uri: &str, value: &Value) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    serde_json::json!({
        "uri": uri,
        "mimeType": "application/json",
        "text": text,
    })
}

pub fn list_resources() -> Value {
    serde_json::json!({
        "resources": [
            as_resource("infra://legend", "legend", "Legend for core terms, aliases, and effect semantics."),
            as_resource("infra://surface/core", "core_surface", "Core-tier canonical discovery surface and preferred tool order."),
            as_resource("infra://schemas/mcp_receipt", "mcp_receipt_schema", "Canonical receipt tool schema."),
            as_resource("infra://schemas/mcp_policy", "mcp_policy_schema", "Canonical policy tool schema."),
            as_resource("infra://schemas/mcp_profile", "mcp_profile_schema", "Canonical profile tool schema."),
            as_resource("infra://schemas/mcp_target", "mcp_target_schema", "Canonical target tool schema."),
            as_resource("infra://capabilities/index", "capabilities", "Visible capability contracts and sources."),
            as_resource("infra://capabilities/families", "capability_families", "Capability families and provider-independent verbs."),
            as_resource("infra://runbooks/index", "runbooks", "Visible runbooks with effects and source metadata."),
            as_resource("infra://receipts/recent", "receipts_recent", "Recent write receipts and statuses."),
            as_resource("infra://operations/recent", "operations_recent", "Recent operation receipts and statuses."),
            as_resource("infra://workspace/summary", "workspace_summary", "Derived AI-facing workspace summary."),
            as_resource("infra://workspace/store", "workspace_store", "Primary store and legacy-path status."),
        ]
    })
}

fn read_schema_resource(uri: &str, tool_name: &str) -> Result<Value, ToolError> {
    let tool = tool_by_name(tool_name).ok_or_else(|| {
        ToolError::not_found(format!("Unknown tool schema: {}", tool_name)).with_hint(
            "Use infra://surface/core or resources/list to inspect known schemas.".to_string(),
        )
    })?;
    Ok(json_content(
        uri,
        &serde_json::json!({
            "name": tool.name.clone(),
            "description": tool.description.clone(),
            "discoveryOnly": tool.discovery_only,
            "inputSchema": tool.input_schema.clone(),
        }),
    ))
}

fn core_surface_resource(uri: &str) -> Value {
    let tools = discovery_tool_catalog()
        .iter()
        .filter(|tool| is_core_discovery_tool(&tool.name))
        .map(|tool| {
            serde_json::json!({
                "name": tool.name.clone(),
                "description": tool.description.clone(),
                "discoveryOnly": tool.discovery_only,
            })
        })
        .collect::<Vec<_>>();
    json_content(
        uri,
        &serde_json::json!({
            "tool_tier": "core",
            "preferred_order": [
                "mcp_capability",
                "mcp_operation",
                "mcp_receipt",
                "mcp_policy",
                "mcp_profile",
                "mcp_target"
            ],
            "tools": tools,
        }),
    )
}

pub async fn read_resource(app: &App, uri: &str) -> Result<Value, ToolError> {
    let content = match uri {
        "infra://legend" => json_content(uri, &legend::build_legend_payload()),
        "infra://surface/core" => core_surface_resource(uri),
        "infra://schemas/mcp_receipt" => read_schema_resource(uri, "mcp_receipt")?,
        "infra://schemas/mcp_policy" => read_schema_resource(uri, "mcp_policy")?,
        "infra://schemas/mcp_profile" => read_schema_resource(uri, "mcp_profile")?,
        "infra://schemas/mcp_target" => read_schema_resource(uri, "mcp_target")?,
        "infra://capabilities/index" => {
            json_content(uri, &app.capability_service.list_capabilities()?)
        }
        "infra://capabilities/families" => {
            json_content(uri, &app.capability_service.families_index()?)
        }
        "infra://runbooks/index" => json_content(
            uri,
            &app.runbook_service.list_runbooks(&ListFilters::default())?,
        ),
        "infra://operations/recent" => {
            json_content(uri, &Value::Array(app.operation_service.list(20, None)?))
        }
        "infra://receipts/recent" => {
            json_content(uri, &Value::Array(app.operation_service.list(20, None)?))
        }
        "infra://workspace/summary" => json_content(
            uri,
            &app.workspace_service
                .summarize(&serde_json::json!({}))
                .await?,
        ),
        "infra://workspace/store" => json_content(
            uri,
            &app.workspace_service
                .store_status(&serde_json::json!({}))
                .await?,
        ),
        _ => {
            return Err(
                ToolError::not_found(format!("Unknown resource URI: {}", uri)).with_hint(
                    "Use resources/list to inspect supported Infra resources.".to_string(),
                ),
            );
        }
    };

    Ok(serde_json::json!({ "contents": [content] }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_catalog_lists_workspace_and_capabilities() {
        let resources = list_resources();
        let uris = resources
            .get("resources")
            .and_then(|v| v.as_array())
            .expect("resources array")
            .iter()
            .filter_map(|v| v.get("uri").and_then(|v| v.as_str()))
            .collect::<Vec<_>>();
        assert!(uris.contains(&"infra://surface/core"));
        assert!(uris.contains(&"infra://schemas/mcp_receipt"));
        assert!(uris.contains(&"infra://schemas/mcp_policy"));
        assert!(uris.contains(&"infra://schemas/mcp_profile"));
        assert!(uris.contains(&"infra://schemas/mcp_target"));
        assert!(uris.contains(&"infra://capabilities/index"));
        assert!(uris.contains(&"infra://capabilities/families"));
        assert!(uris.contains(&"infra://receipts/recent"));
        assert!(uris.contains(&"infra://operations/recent"));
        assert!(uris.contains(&"infra://workspace/store"));
    }

    #[test]
    fn core_surface_resource_prefers_new_canonical_tools() {
        let content = core_surface_resource("infra://surface/core");
        let text = content
            .get("text")
            .and_then(|v| v.as_str())
            .expect("resource text");
        let parsed: Value = serde_json::from_str(text).expect("core surface json");
        let tool_names = parsed
            .get("tools")
            .and_then(|v| v.as_array())
            .expect("tools array")
            .iter()
            .filter_map(|item| item.get("name").and_then(|v| v.as_str()))
            .collect::<Vec<_>>();
        assert!(tool_names.contains(&"mcp_receipt"));
        assert!(tool_names.contains(&"mcp_policy"));
        assert!(tool_names.contains(&"mcp_profile"));
        assert!(tool_names.contains(&"mcp_target"));
        assert!(!tool_names.contains(&"mcp_project"));
    }

    #[test]
    fn schema_resource_uses_discovery_only_key() {
        let content = read_schema_resource("infra://schemas/mcp_receipt", "mcp_receipt")
            .expect("schema resource");
        let text = content
            .get("text")
            .and_then(|v| v.as_str())
            .expect("resource text");
        let parsed: Value = serde_json::from_str(text).expect("schema json");
        assert!(parsed.get("discoveryOnly").is_some());
    }
}
