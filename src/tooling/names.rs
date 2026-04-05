use once_cell::sync::Lazy;
use std::collections::HashMap;

pub const CANONICAL_TOOL_NAMES: &[&str] = &[
    "alias",
    "api",
    "artifacts",
    "audit",
    "capability",
    "context",
    "env",
    "evidence",
    "intent",
    "job",
    "local",
    "operation",
    "pipeline",
    "policy",
    "preset",
    "profile",
    "project",
    "receipt",
    "repo",
    "runbook",
    "sql",
    "ssh",
    "state",
    "target",
    "vault",
    "workspace",
];

pub const BUILTIN_TOOL_ALIASES: &[(&str, &str)] =
    &[("http", "api"), ("psql", "sql"), ("postgres", "sql")];

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
        .map(|(alias, target)| ((*alias).to_string(), (*target).to_string()))
        .collect()
}

pub fn is_canonical_tool_name(tool: &str) -> bool {
    CANONICAL_TOOL_NAMES.contains(&tool)
}
