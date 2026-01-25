use once_cell::sync::Lazy;
use std::collections::HashMap;

pub const BUILTIN_TOOL_ALIASES: &[(&str, &str)] = &[
    ("sql", "mcp_psql_manager"),
    ("psql", "mcp_psql_manager"),
    ("ssh", "mcp_ssh_manager"),
    ("job", "mcp_jobs"),
    ("artifacts", "mcp_artifacts"),
    ("http", "mcp_api_client"),
    ("api", "mcp_api_client"),
    ("repo", "mcp_repo"),
    ("state", "mcp_state"),
    ("project", "mcp_project"),
    ("context", "mcp_context"),
    ("workspace", "mcp_workspace"),
    ("env", "mcp_env"),
    ("vault", "mcp_vault"),
    ("runbook", "mcp_runbook"),
    ("capability", "mcp_capability"),
    ("intent", "mcp_intent"),
    ("evidence", "mcp_evidence"),
    ("alias", "mcp_alias"),
    ("preset", "mcp_preset"),
    ("audit", "mcp_audit"),
    ("pipeline", "mcp_pipeline"),
    ("local", "mcp_local"),
];

static BUILTIN_TOOL_ALIAS_MAP: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut map = HashMap::new();
    for (alias, target) in BUILTIN_TOOL_ALIASES {
        map.insert(*alias, *target);
    }
    map
});

pub fn builtin_tool_aliases() -> &'static [(&'static str, &'static str)] {
    BUILTIN_TOOL_ALIASES
}

pub fn builtin_tool_alias_map() -> &'static HashMap<&'static str, &'static str> {
    &BUILTIN_TOOL_ALIAS_MAP
}

pub fn canonical_tool_name(tool: &str) -> &str {
    builtin_tool_alias_map().get(tool).copied().unwrap_or(tool)
}

pub fn builtin_tool_alias_map_owned() -> HashMap<String, String> {
    BUILTIN_TOOL_ALIASES
        .iter()
        .map(|(alias, target)| (alias.to_string(), target.to_string()))
        .collect()
}
