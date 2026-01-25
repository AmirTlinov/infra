mod examples;
mod search;
mod summaries;

use crate::app::App;
use crate::mcp::aliases::builtin_tool_alias_map;
use crate::mcp::catalog::tool_by_name;
use crate::mcp::legend;
use crate::utils::arg_aliases::action_aliases_for_tool;
use crate::utils::feature_flags::is_unsafe_local_enabled;
use crate::utils::listing::ListFilters;
use crate::utils::suggest::suggest;
use examples::build_tool_example;
use search::{build_help_query_payload, resolve_user_aliases};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use summaries::{is_core_tool, summaries_ordered, Summary};

fn resolve_tool_tier() -> String {
    let raw = std::env::var("INFRA_TOOL_TIER").unwrap_or_else(|_| "full".to_string());
    let normalized = raw.trim().to_lowercase();
    if normalized == "core" {
        "core".to_string()
    } else {
        "full".to_string()
    }
}

fn extract_actions(tool_name: &str) -> Vec<String> {
    tool_by_name(tool_name)
        .and_then(|tool| tool.input_schema.get("properties"))
        .and_then(|props| props.get("action"))
        .and_then(|action| action.get("enum"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn extract_fields(tool_name: &str) -> Vec<String> {
    let ignored: HashSet<&str> = HashSet::from([
        "action",
        "output",
        "store_as",
        "store_scope",
        "trace_id",
        "span_id",
        "parent_span_id",
        "preset",
        "preset_name",
        "response_mode",
    ]);
    tool_by_name(tool_name)
        .and_then(|tool| tool.input_schema.get("properties"))
        .and_then(|props| props.as_object())
        .map(|map| {
            map.keys()
                .filter(|key| !ignored.contains(key.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn summaries_map(ordered: &[(&'static str, Summary)]) -> HashMap<&'static str, Summary> {
    ordered.iter().map(|(k, v)| (*k, *v)).collect()
}

fn help_overview(tier: &str) -> &'static str {
    if tier == "core" {
        "Infra (tool_tier=core): используйте workspace/jobs/artifacts (и project опционально); остальные инструменты скрыты из tools/list, но доступны при явном вызове."
    } else if is_unsafe_local_enabled() {
        "Infra подключает PostgreSQL, SSH, HTTP, state, project, context, runbook, capability/intent/evidence, alias, preset, audit, pipeline и (unsafe) local инструменты."
    } else {
        "Infra подключает PostgreSQL, SSH, HTTP, state, project, context, runbook, capability/intent/evidence, alias, preset, audit и pipeline инструменты."
    }
}

pub fn build_help_payload(app: &App, args: &Value) -> Value {
    let tool_aliases = builtin_tool_alias_map();
    let raw_tool = args
        .get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let raw_action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let raw_query = args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let tier = resolve_tool_tier();
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .map(|v| v.clamp(1, 50) as usize)
        .unwrap_or(20);

    let tool = if raw_tool.is_empty() {
        String::new()
    } else {
        tool_aliases
            .get(raw_tool.as_str())
            .copied()
            .unwrap_or(raw_tool.as_str())
            .to_string()
    };

    let ordered = summaries_ordered();
    let summaries = summaries_map(&ordered);

    let user_aliases = app
        .alias_service
        .as_ref()
        .and_then(|svc| svc.list_aliases(&ListFilters::default()).ok())
        .map(|value| resolve_user_aliases(Some(value)))
        .unwrap_or_default();

    if tool.is_empty() && !raw_query.is_empty() {
        return build_help_query_payload(&raw_query, limit, &tier, &user_aliases);
    }

    if !tool.is_empty() {
        if tool == "legend" {
            return legend::build_legend_payload();
        }

        if !summaries.contains_key(tool.as_str()) {
            let mut known_tools: Vec<String> =
                ordered.iter().map(|(k, _)| (*k).to_string()).collect();
            for key in tool_aliases.keys() {
                known_tools.push((*key).to_string());
            }
            known_tools.sort();
            known_tools.dedup();
            let suggestions = if raw_tool.is_empty() {
                Vec::new()
            } else {
                suggest(&raw_tool, &known_tools, 5)
            };
            let mut obj = serde_json::Map::new();
            obj.insert(
                "error".to_string(),
                Value::String(format!("Неизвестный инструмент: {}", tool)),
            );
            obj.insert(
                "known_tools".to_string(),
                Value::Array(known_tools.into_iter().map(Value::String).collect()),
            );
            if !suggestions.is_empty() {
                obj.insert(
                    "did_you_mean".to_string(),
                    Value::Array(suggestions.into_iter().map(Value::String).collect()),
                );
            }
            obj.insert(
                "hint".to_string(),
                Value::String(
                    "Попробуйте: { tool: 'mcp_ssh_manager' } или { tool: 'ssh' }".to_string(),
                ),
            );
            return Value::Object(obj);
        }

        let actions = extract_actions(&tool);
        let fields = extract_fields(&tool);
        let summary = summaries.get(tool.as_str()).copied().unwrap_or(Summary {
            description: "",
            usage: "",
        });
        let mut entry = serde_json::json!({
            "name": tool,
            "description": summary.description,
            "usage": summary.usage,
            "actions": actions,
            "fields": fields,
            "hint": if raw_action.is_empty() { format!("help({{ tool: '{}', action: '<action>' }})", tool) } else { format!("help({{ tool: '{}', action: '{}' }})", tool, raw_action) },
        });

        let alias_pairs = action_aliases_for_tool(&tool);
        if !alias_pairs.is_empty() {
            if let Some(obj) = entry.as_object_mut() {
                let mut alias_map = serde_json::Map::new();
                for (alias, canonical) in alias_pairs {
                    alias_map.insert(alias, Value::String(canonical));
                }
                obj.insert("action_aliases".to_string(), Value::Object(alias_map));
            }
        }

        if !raw_action.is_empty() {
            let actions: Vec<String> = entry
                .get("actions")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if !actions.is_empty() && !actions.contains(&raw_action) {
                let suggestions = suggest(&raw_action, &actions, 5);
                let mut obj = serde_json::Map::new();
                obj.insert(
                    "name".to_string(),
                    entry.get("name").cloned().unwrap_or(Value::Null),
                );
                obj.insert(
                    "description".to_string(),
                    entry.get("description").cloned().unwrap_or(Value::Null),
                );
                obj.insert(
                    "usage".to_string(),
                    entry.get("usage").cloned().unwrap_or(Value::Null),
                );
                obj.insert(
                    "actions".to_string(),
                    entry.get("actions").cloned().unwrap_or(Value::Null),
                );
                obj.insert(
                    "fields".to_string(),
                    entry.get("fields").cloned().unwrap_or(Value::Null),
                );
                obj.insert(
                    "hint".to_string(),
                    entry.get("hint").cloned().unwrap_or(Value::Null),
                );
                obj.insert(
                    "error".to_string(),
                    Value::String(format!("Неизвестный action для {}: {}", tool, raw_action)),
                );
                obj.insert(
                    "known_actions".to_string(),
                    Value::Array(actions.iter().map(|s| Value::String(s.clone())).collect()),
                );
                if !suggestions.is_empty() {
                    obj.insert(
                        "did_you_mean_actions".to_string(),
                        Value::Array(suggestions.into_iter().map(Value::String).collect()),
                    );
                }
                return Value::Object(obj);
            }

            return serde_json::json!({
                "name": entry.get("name").cloned().unwrap_or(Value::Null),
                "description": entry.get("description").cloned().unwrap_or(Value::Null),
                "usage": entry.get("usage").cloned().unwrap_or(Value::Null),
                "actions": entry.get("actions").cloned().unwrap_or(Value::Null),
                "fields": entry.get("fields").cloned().unwrap_or(Value::Null),
                "action_aliases": entry.get("action_aliases").cloned().unwrap_or(Value::Null),
                "hint": entry.get("hint").cloned().unwrap_or(Value::Null),
                "action": raw_action,
                "example": build_tool_example(&tool, &raw_action),
            });
        }

        let key_examples: HashMap<&str, Vec<&str>> = HashMap::from([
            ("mcp_repo", vec!["repo_info", "exec", "apply_patch"]),
            (
                "mcp_ssh_manager",
                vec!["exec", "exec_follow", "deploy_file"],
            ),
            ("mcp_artifacts", vec!["get", "list"]),
            ("mcp_jobs", vec!["follow_job", "tail_job"]),
            ("mcp_workspace", vec!["summary", "suggest", "run"]),
            ("mcp_context", vec!["summary", "refresh"]),
            ("mcp_project", vec!["project_upsert", "project_use"]),
            ("mcp_psql_manager", vec!["query", "select"]),
            ("mcp_api_client", vec!["request", "smoke_http"]),
        ]);

        let chosen = key_examples
            .get(tool.as_str())
            .map(|acts| {
                acts.iter()
                    .filter(|act| actions.contains(&act.to_string()))
                    .copied()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| actions.iter().take(3).map(|s| s.as_str()).collect());

        let examples = chosen
            .iter()
            .take(4)
            .filter_map(|act| {
                build_tool_example(&tool, act)
                    .map(|ex| serde_json::json!({"action": act, "example": ex}))
            })
            .collect::<Vec<_>>();

        let mut obj = serde_json::Map::new();
        obj.insert(
            "name".to_string(),
            entry.get("name").cloned().unwrap_or(Value::Null),
        );
        obj.insert(
            "description".to_string(),
            entry.get("description").cloned().unwrap_or(Value::Null),
        );
        obj.insert(
            "usage".to_string(),
            entry.get("usage").cloned().unwrap_or(Value::Null),
        );
        obj.insert(
            "actions".to_string(),
            entry.get("actions").cloned().unwrap_or(Value::Null),
        );
        obj.insert(
            "fields".to_string(),
            entry.get("fields").cloned().unwrap_or(Value::Null),
        );
        obj.insert(
            "hint".to_string(),
            entry.get("hint").cloned().unwrap_or(Value::Null),
        );
        if !examples.is_empty() {
            obj.insert("examples".to_string(), Value::Array(examples));
        }
        obj.insert(
            "legend_hint".to_string(),
            Value::String("См. `legend()` для семантики общих полей (`output`, `store_as`, `preset`, `project/target`).".to_string()),
        );
        return Value::Object(obj);
    }

    let visible: Vec<(&str, Summary)> = ordered
        .iter()
        .filter_map(|(name, summary)| {
            if tier == "core" && !is_core_tool(name) {
                return None;
            }
            Some((*name, *summary))
        })
        .collect();

    serde_json::json!({
        "overview": help_overview(&tier),
        "usage": "help({ tool: 'mcp_ssh_manager' }) или help({ tool: 'mcp_ssh_manager', action: 'exec' })",
        "legend": {
            "hint": "Вся семантика общих полей и правил resolution — в `legend()` (или `help({ tool: 'legend' })`).",
            "includes": ["common_fields", "resolution", "refs", "safety", "golden_path"],
        },
        "tools": visible.iter().map(|(name, summary)| serde_json::json!({
            "name": name,
            "description": summary.description,
            "usage": summary.usage,
            "actions": extract_actions(name),
        })).collect::<Vec<_>>(),
    })
}
