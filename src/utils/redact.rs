use crate::utils::text::truncate_utf8_prefix;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;

const DEFAULT_REDACTION: &str = "[REDACTED]";
const INLINE_REDACTION: &str = "***REDACTED***";

static SENSITIVE_KEYS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "password",
        "passphrase",
        "private_key",
        "public_key",
        "secret",
        "token",
        "api_key",
        "auth_token",
        "auth_password",
        "client_secret",
        "refresh_token",
        "header_value",
        "authorization",
        "encryption_key",
    ]
    .into_iter()
    .collect()
});

static SENSITIVE_HEADER_KEYS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "authorization",
        "proxy-authorization",
        "x-api-key",
        "x-auth-token",
        "x-access-token",
    ]
    .into_iter()
    .collect()
});

static INLINE_REDACTION_PATTERNS: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    vec![
        (
            Regex::new(r"\bsk-proj-[A-Za-z0-9_-]{10,}\b").expect("inline redaction regex"),
            "sk-proj-***REDACTED***",
        ),
        (
            Regex::new(r"\bsk-[A-Za-z0-9_-]{10,}\b").expect("inline redaction regex"),
            "sk-***REDACTED***",
        ),
        (
            Regex::new(r"\bghp_[A-Za-z0-9]{20,}\b").expect("inline redaction regex"),
            "ghp_***REDACTED***",
        ),
        (
            Regex::new(r"\bgithub_pat_[A-Za-z0-9_]{20,}\b").expect("inline redaction regex"),
            "github_pat_***REDACTED***",
        ),
        (
            Regex::new(r"\bglpat-[A-Za-z0-9_-]{10,}\b").expect("inline redaction regex"),
            "glpat-***REDACTED***",
        ),
        (
            Regex::new(r"\bxox[baprs]-[0-9A-Za-z-]{10,}\b").expect("inline redaction regex"),
            INLINE_REDACTION,
        ),
        (
            Regex::new(r"\bAIza[0-9A-Za-z_-]{20,}\b").expect("inline redaction regex"),
            "AIza***REDACTED***",
        ),
        (
            Regex::new(r"\beyJ[a-zA-Z0-9_-]{10,}\.[a-zA-Z0-9_-]{10,}\.[a-zA-Z0-9_-]{10,}\b")
                .expect("inline redaction regex"),
            INLINE_REDACTION,
        ),
        (
            Regex::new(r"\b(Bearer)\s+([A-Za-z0-9._~-]{10,})\b").expect("inline redaction regex"),
            "$1 ***REDACTED***",
        ),
        (
            Regex::new(r"\b(AKIA|ASIA)[0-9A-Z]{16}\b").expect("inline redaction regex"),
            "AKIA***REDACTED***",
        ),
        (
            Regex::new(r#"\b(password|passwd|passphrase|token|api[_-]?key|secret|access[_-]?token|refresh[_-]?token)\b\s*([:=])\s*([^\s"'`]+)"#)
                .expect("inline redaction regex"),
            "$1$2***REDACTED***",
        ),
        (
            Regex::new(
                r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
            )
            .expect("inline redaction regex"),
            "-----BEGIN PRIVATE KEY-----\n***REDACTED***\n-----END PRIVATE KEY-----",
        ),
    ]
});

fn normalize_key(key: &str) -> String {
    key.trim().to_lowercase()
}

pub fn is_sensitive_key(key: &str) -> bool {
    let normalized = normalize_key(key);
    if normalized.is_empty() {
        return false;
    }
    if SENSITIVE_KEYS.contains(normalized.as_str()) {
        return true;
    }
    normalized.contains("secret") || normalized.contains("token")
}

fn truncate_string(value: &str, max_length: usize) -> String {
    if max_length == usize::MAX {
        return value.to_string();
    }
    if max_length == 0 {
        return "".to_string();
    }
    if value.len() <= max_length {
        return value.to_string();
    }
    format!("{}...", truncate_utf8_prefix(value, max_length))
}

fn redact_inline_secrets(value: &str, extra: Option<&[String]>) -> String {
    let mut out = value.to_string();
    for (re, replacement) in INLINE_REDACTION_PATTERNS.iter() {
        if re.is_match(&out) {
            out = re.replace_all(&out, *replacement).to_string();
        }
    }

    if let Some(values) = extra {
        for raw in values {
            let needle = raw.trim();
            if needle.len() < 6 {
                continue;
            }
            out = out.replace(needle, INLINE_REDACTION);
        }
    }

    out
}

pub fn redact_text(value: &str, max_string: usize, extra_secrets: Option<&[String]>) -> String {
    let redacted = redact_inline_secrets(value, extra_secrets);
    truncate_string(&redacted, max_string)
}

fn redact_headers(value: &Value, max_string: usize, extra: Option<&[String]>) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(map) = value.as_object() {
        for (key, entry) in map.iter() {
            let normalized = normalize_key(key);
            if SENSITIVE_HEADER_KEYS.contains(normalized.as_str()) {
                out.insert(key.clone(), Value::String(DEFAULT_REDACTION.to_string()));
            } else if let Some(text) = entry.as_str() {
                out.insert(
                    key.clone(),
                    Value::String(redact_text(text, max_string, extra)),
                );
            } else {
                out.insert(key.clone(), entry.clone());
            }
        }
    }
    Value::Object(out)
}

fn redact_map_values(value: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(map) = value.as_object() {
        for (key, _) in map.iter() {
            out.insert(key.clone(), Value::String(DEFAULT_REDACTION.to_string()));
        }
    }
    Value::Object(out)
}

pub fn redact_object(value: &Value, max_string: usize, extra_secrets: Option<&[String]>) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::String(text) => Value::String(redact_text(text, max_string, extra_secrets)),
        Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_object(item, max_string, extra_secrets))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, entry) in map.iter() {
                if key == "headers" {
                    out.insert(
                        key.clone(),
                        redact_headers(entry, max_string, extra_secrets),
                    );
                    continue;
                }
                let normalized = normalize_key(key);
                if (normalized == "env" || normalized == "variables") && entry.is_object() {
                    out.insert(key.clone(), redact_map_values(entry));
                    continue;
                }
                if is_sensitive_key(key) {
                    out.insert(key.clone(), Value::String(DEFAULT_REDACTION.to_string()));
                    continue;
                }
                out.insert(key.clone(), redact_object(entry, max_string, extra_secrets));
            }
            Value::Object(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::redact_object;
    use serde_json::Value;

    #[test]
    fn redact_object_redacts_env_map_values() {
        let input = serde_json::json!({"env": {"TOKEN": "abc", "FOO": "bar"}});
        let out = redact_object(&input, usize::MAX, None);
        assert_eq!(out["env"]["TOKEN"], Value::String("[REDACTED]".to_string()));
        assert_eq!(out["env"]["FOO"], Value::String("[REDACTED]".to_string()));
    }

    #[test]
    fn redact_object_preserves_non_map_env_field() {
        let input = serde_json::json!({"env": "mcp_env"});
        let out = redact_object(&input, usize::MAX, None);
        assert_eq!(out["env"], Value::String("mcp_env".to_string()));
    }

    #[test]
    fn redact_object_preserves_non_map_variables_field() {
        let input = serde_json::json!({"variables": "not-a-map"});
        let out = redact_object(&input, usize::MAX, None);
        assert_eq!(out["variables"], Value::String("not-a-map".to_string()));
    }
}
