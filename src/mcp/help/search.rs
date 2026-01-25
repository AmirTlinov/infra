use crate::mcp::aliases::builtin_tool_aliases;
use crate::mcp::catalog::tool_by_name;
use crate::mcp::help::examples::build_tool_example;
use crate::mcp::help::summaries::{help_hint, primary_tool_alias, summaries_ordered};
use crate::utils::suggest::suggest;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct UserAlias {
    pub name: String,
    pub tool: String,
    pub description: Option<String>,
}

fn normalize_token(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    let n = a.len();
    let m = b.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }

    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0; m + 1];
    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        prev.clone_from_slice(&curr);
    }
    prev[m]
}

fn max_allowed_distance(input: &str) -> usize {
    let normalized = normalize_token(input);
    if normalized.is_empty() {
        return 0;
    }
    if normalized.len() <= 4 {
        return 1;
    }
    if normalized.len() <= 8 {
        return 2;
    }
    (normalized.len() as f32 * 0.35).floor().max(3.0) as usize
}

fn term_score(term: &str, token: &str) -> Option<usize> {
    let t = normalize_token(term);
    let cand = normalize_token(token);
    if t.is_empty() || cand.is_empty() {
        return None;
    }
    if cand.contains(&t) {
        return Some(if cand.starts_with(&t) { 0 } else { 1 });
    }
    let dist = levenshtein(&t, &cand);
    let allowed = max_allowed_distance(&t);
    if dist <= allowed {
        Some(10 + dist)
    } else {
        None
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

fn terms_from_query(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .filter(|s| !s.trim().is_empty())
        .take(6)
        .map(|s| s.to_string())
        .collect()
}

fn visible_tool_names(tier: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for (tool, _) in summaries_ordered() {
        if tier == "core" && !crate::mcp::help::summaries::is_core_tool(tool) {
            continue;
        }
        out.insert(tool.to_string());
    }
    out
}

pub fn build_help_query_payload(
    query: &str,
    limit: usize,
    tier: &str,
    user_aliases: &[UserAlias],
) -> Value {
    let terms = terms_from_query(query);
    let visible = visible_tool_names(tier);

    let mut results = Vec::<Value>::new();

    for (tool_name, summary) in summaries_ordered() {
        let alias = primary_tool_alias(tool_name).map(|s| s.to_string());
        let tokens = [
            tool_name,
            primary_tool_alias(tool_name).unwrap_or(""),
            summary.description,
        ]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>();
        results.push(serde_json::json!({
            "kind": "tool",
            "tool": tool_name,
            "alias": alias,
            "exposed_in_tools_list": visible.contains(tool_name),
            "description": summary.description,
            "tokens": tokens,
        }));

        for action_name in extract_actions(tool_name) {
            let display = primary_tool_alias(tool_name).unwrap_or(tool_name);
            results.push(serde_json::json!({
                "kind": "action",
                "tool": tool_name,
                "alias": primary_tool_alias(tool_name),
                "action": action_name,
                "exposed_in_tools_list": visible.contains(tool_name),
                "hint": help_hint(tool_name, Some(action_name.as_str())),
                "tokens": [action_name.as_str(), format!("{}.{}", tool_name, action_name), format!("{}.{}", display, action_name)],
            }));
        }

        for field_name in extract_fields(tool_name) {
            let display = primary_tool_alias(tool_name).unwrap_or(tool_name);
            results.push(serde_json::json!({
                "kind": "field",
                "tool": tool_name,
                "alias": primary_tool_alias(tool_name),
                "field": field_name,
                "exposed_in_tools_list": visible.contains(tool_name),
                "hint": help_hint(tool_name, None),
                "tokens": [field_name.as_str(), format!("{}.{}", tool_name, field_name), format!("{}.{}", display, field_name)],
            }));
        }
    }

    for (alias_name, tool_name) in builtin_tool_aliases().iter() {
        results.push(serde_json::json!({
            "kind": "tool_alias",
            "alias": alias_name,
            "tool": tool_name,
            "exposed_in_tools_list": visible.contains(*tool_name),
            "hint": help_hint(tool_name, None),
            "tokens": [alias_name.to_string(), tool_name.to_string()],
        }));
    }

    for entry in user_aliases.iter() {
        let mut obj = serde_json::Map::new();
        obj.insert("kind".to_string(), Value::String("alias".to_string()));
        obj.insert("alias".to_string(), Value::String(entry.name.clone()));
        obj.insert("tool".to_string(), Value::String(entry.tool.clone()));
        if let Some(description) = entry.description.as_ref() {
            obj.insert(
                "description".to_string(),
                Value::String(description.clone()),
            );
        }
        obj.insert(
            "exposed_in_tools_list".to_string(),
            Value::Bool(visible.contains(&entry.tool)),
        );
        let tokens = [
            entry.name.clone(),
            entry.tool.clone(),
            entry.description.clone().unwrap_or_default(),
        ]
        .into_iter()
        .filter(|t| !t.trim().is_empty())
        .map(Value::String)
        .collect::<Vec<_>>();
        obj.insert("tokens".to_string(), Value::Array(tokens));
        results.push(Value::Object(obj));
    }

    let mut scored = Vec::<(Value, usize)>::new();
    for item in results {
        let tokens: Vec<String> = item
            .get("tokens")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let mut total = 0usize;
        let mut ok = true;
        for term in terms.iter() {
            let mut best: Option<usize> = None;
            for token in tokens.iter() {
                if let Some(score) = term_score(term, token) {
                    best = Some(best.map_or(score, |b| b.min(score)));
                }
            }
            if let Some(best) = best {
                total += best;
            } else {
                ok = false;
                break;
            }
        }
        if ok {
            scored.push((item, total));
        }
    }

    scored.sort_by(|a, b| a.1.cmp(&b.1));

    let mut out = Vec::<Value>::new();
    for (mut item, _) in scored {
        if item.get("kind").and_then(|v| v.as_str()) == Some("action") {
            if let (Some(tool), Some(action)) = (
                item.get("tool").and_then(|v| v.as_str()),
                item.get("action").and_then(|v| v.as_str()),
            ) {
                if let Some(example) = build_tool_example(tool, action) {
                    if let Some(obj) = item.as_object_mut() {
                        obj.insert("example".to_string(), example);
                    }
                }
            }
        }
        if let Some(obj) = item.as_object_mut() {
            obj.remove("tokens");
        }
        out.push(item);
        if out.len() >= limit {
            break;
        }
    }

    if out.is_empty() {
        let mut known_tools: Vec<String> = summaries_ordered()
            .iter()
            .map(|(k, _)| (*k).to_string())
            .collect();
        for (alias, tool) in builtin_tool_aliases().iter() {
            known_tools.push((*alias).to_string());
            known_tools.push((*tool).to_string());
        }
        for entry in user_aliases.iter() {
            known_tools.push(entry.name.clone());
        }
        known_tools.sort();
        known_tools.dedup();

        let suggestions = suggest(query, &known_tools, 5);
        let mut obj = serde_json::Map::new();
        obj.insert("query".to_string(), Value::String(query.to_string()));
        obj.insert("limit".to_string(), Value::Number((limit as i64).into()));
        obj.insert("results".to_string(), Value::Array(Vec::new()));
        if !suggestions.is_empty() {
            obj.insert(
                "did_you_mean".to_string(),
                Value::Array(suggestions.into_iter().map(Value::String).collect()),
            );
        }
        obj.insert(
            "hint".to_string(),
            Value::String(
                "Попробуйте: help({ tool: 'ssh' }) или help({ tool: 'ssh', action: 'exec' })"
                    .to_string(),
            ),
        );
        return Value::Object(obj);
    }

    serde_json::json!({
        "query": query,
        "limit": limit,
        "results": out,
        "hint": "Для деталей используйте help({ tool: '<tool>', action: '<action>' })",
    })
}

pub fn resolve_user_aliases(raw: Option<Value>) -> Vec<UserAlias> {
    let mut out = Vec::new();
    let Some(listed) = raw else {
        return out;
    };
    let Some(items) = listed.get("aliases").and_then(|v| v.as_array()) else {
        return out;
    };

    for entry in items {
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let tool = entry
            .get("tool")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if name.is_empty() || tool.is_empty() {
            continue;
        }
        let description = entry
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        out.push(UserAlias {
            name: name.to_string(),
            tool: tool.to_string(),
            description,
        });
    }

    out
}
