use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Effects {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>, // "read" | "write" | "mixed"
    #[serde(default)]
    pub requires_apply: bool,
    #[serde(default)]
    pub irreversible: bool,
}

impl Effects {
    pub fn to_value(&self) -> Value {
        let mut map = serde_json::Map::new();
        if let Some(kind) = &self.kind {
            map.insert("kind".to_string(), Value::String(kind.clone()));
        }
        map.insert(
            "requires_apply".to_string(),
            Value::Bool(self.requires_apply),
        );
        map.insert("irreversible".to_string(), Value::Bool(self.irreversible));
        Value::Object(map)
    }
}

pub fn resolve_effects(meta: &Value) -> Effects {
    let mut out = Effects::default();

    let mut kind_set = false;
    let mut requires_apply_set = false;
    let mut irreversible_set = false;

    if let Some(effects) = meta.get("effects").and_then(|v| v.as_object()) {
        if let Some(kind) = effects.get("kind").and_then(|v| v.as_str()) {
            out.kind = Some(kind.to_string());
            kind_set = true;
        }
        if let Some(flag) = effects.get("requires_apply").and_then(|v| v.as_bool()) {
            out.requires_apply = flag;
            requires_apply_set = true;
        }
        if let Some(flag) = effects.get("irreversible").and_then(|v| v.as_bool()) {
            out.irreversible = flag;
            irreversible_set = true;
        }
    }

    // If kind is explicitly set but requires_apply isn't, default conservatively.
    if kind_set && !requires_apply_set && matches!(out.kind.as_deref(), Some("write" | "mixed")) {
        out.requires_apply = true;
    }

    // Fill missing fields from tags (do not override explicit effects).
    let tags = meta
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_lowercase())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let has_tag = |needle: &str| tags.iter().any(|t| t == needle);

    if has_tag("irreversible") && !irreversible_set {
        out.irreversible = true;
    }

    if has_tag("write") {
        if !kind_set {
            out.kind = Some("write".to_string());
            kind_set = true;
        }
        if !requires_apply_set {
            out.requires_apply = true;
        }
    }

    if has_tag("mixed") {
        if !kind_set {
            out.kind = Some("mixed".to_string());
            kind_set = true;
        }
        if !requires_apply_set {
            out.requires_apply = true;
        }
    }

    if has_tag("read") && !kind_set {
        out.kind = Some("read".to_string());
    }

    // Irreversible implies apply unless explicitly disabled (rare).
    if out.irreversible && !requires_apply_set {
        out.requires_apply = true;
    }

    out
}
