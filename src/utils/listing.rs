use serde_json::{Map, Value};

const MAX_LIST_LIMIT: usize = 500;

#[derive(Clone, Debug, Default)]
pub struct ListFilters {
    pub limit: Option<usize>,
    pub offset: usize,
    pub query: Option<String>,
    pub tags: Vec<String>,
    pub where_eq: Map<String, Value>,
}

#[derive(Clone, Debug)]
pub struct ListResult {
    pub total: usize,
    pub items: Vec<Value>,
}

impl ListFilters {
    pub fn from_args(args: &Value) -> Self {
        let mut out = ListFilters::default();
        let Some(map) = args.as_object() else {
            return out;
        };
        out.limit = parse_usize(map.get("limit")).map(|v| v.min(MAX_LIST_LIMIT));
        out.offset = parse_usize(map.get("offset")).unwrap_or(0);
        out.query = map
            .get("query")
            .and_then(|v| v.as_str())
            .map(normalize_text)
            .filter(|v| !v.is_empty());
        out.tags = parse_tags(map.get("tags"));
        if let Some(where_obj) = map.get("where").and_then(|v| v.as_object()) {
            out.where_eq = where_obj.clone();
        }
        out
    }

    pub fn apply(
        &self,
        items: Vec<Value>,
        query_fields: &[&str],
        tag_field: Option<&str>,
    ) -> ListResult {
        let mut filtered: Vec<Value> = items
            .into_iter()
            .filter(|item| {
                if let Some(map) = item.as_object() {
                    if let Some(query) = self.query.as_ref() {
                        if !matches_query(map, query, query_fields) {
                            return false;
                        }
                    }
                    if !self.tags.is_empty() && !matches_tags(map, &self.tags, tag_field) {
                        return false;
                    }
                    if !self.where_eq.is_empty() && !matches_where(map, &self.where_eq) {
                        return false;
                    }
                    return true;
                }
                if let Some(text) = item.as_str() {
                    if let Some(query) = self.query.as_ref() {
                        let hay = normalize_text(text);
                        if !hay.contains(query) {
                            return false;
                        }
                    }
                    return self.tags.is_empty() && self.where_eq.is_empty();
                }
                if let Some(query) = self.query.as_ref() {
                    let hay = normalize_text(&item.to_string());
                    if !hay.contains(query) {
                        return false;
                    }
                }
                if !self.tags.is_empty() || !self.where_eq.is_empty() {
                    return false;
                }
                true
            })
            .collect();

        let total = filtered.len();
        let start = self.offset.min(total);
        let end = match self.limit {
            Some(limit) => (start + limit).min(total),
            None => total,
        };
        let items = if start >= end {
            Vec::new()
        } else {
            filtered.drain(start..end).collect()
        };

        ListResult { total, items }
    }

    pub fn meta(&self, total: usize, returned: usize) -> Value {
        serde_json::json!({
            "total": total,
            "returned": returned,
            "offset": self.offset,
            "limit": self
                .limit
                .map(|v| Value::Number(serde_json::Number::from(v as i64)))
                .unwrap_or(Value::Null),
        })
    }
}

fn parse_usize(value: Option<&Value>) -> Option<usize> {
    let value = value?;
    if let Some(n) = value.as_i64() {
        if n >= 0 {
            return Some(n as usize);
        }
    }
    if let Some(text) = value.as_str() {
        if let Ok(parsed) = text.parse::<i64>() {
            if parsed >= 0 {
                return Some(parsed as usize);
            }
        }
    }
    None
}

fn parse_tags(value: Option<&Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Some(text) = value.as_str() {
        return text
            .replace(',', " ")
            .split_whitespace()
            .map(normalize_text)
            .filter(|v| !v.is_empty())
            .collect();
    }
    if let Some(arr) = value.as_array() {
        return arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(normalize_text)
            .filter(|v| !v.is_empty())
            .collect();
    }
    Vec::new()
}

fn normalize_text(value: &str) -> String {
    value.trim().to_lowercase()
}

fn value_as_strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => vec![normalize_text(text)],
        Value::Number(num) => vec![num.to_string()],
        Value::Bool(value) => vec![value.to_string()],
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(normalize_text)
            .collect(),
        _ => Vec::new(),
    }
}

fn matches_query(item: &Map<String, Value>, query: &str, fields: &[&str]) -> bool {
    if fields.is_empty() {
        return false;
    }
    for field in fields {
        if let Some(value) = item.get(*field) {
            let parts = value_as_strings(value);
            for part in parts {
                if part.contains(query) {
                    return true;
                }
            }
        }
    }
    false
}

fn matches_tags(item: &Map<String, Value>, tags: &[String], field: Option<&str>) -> bool {
    let Some(field) = field else {
        return false;
    };
    let Some(value) = item.get(field) else {
        return false;
    };
    let mut available = value_as_strings(value);
    if available.is_empty() {
        return false;
    }
    available.sort();
    available.dedup();
    tags.iter().all(|tag| available.binary_search(tag).is_ok())
}

fn matches_where(item: &Map<String, Value>, filters: &Map<String, Value>) -> bool {
    for (key, expected) in filters {
        let Some(actual) = item.get(key) else {
            return false;
        };
        if !values_match(expected, actual) {
            return false;
        }
    }
    true
}

fn values_match(expected: &Value, actual: &Value) -> bool {
    match expected {
        Value::String(text) => {
            let expected_norm = normalize_text(text);
            let actual_strings = value_as_strings(actual);
            actual_strings.iter().any(|val| val == &expected_norm)
        }
        Value::Number(num) => match actual {
            Value::Number(other) => other == num,
            Value::String(text) => {
                if let Some(expected) = num.as_f64() {
                    text.parse::<f64>()
                        .ok()
                        .map(|v| v == expected)
                        .unwrap_or(false)
                } else {
                    false
                }
            }
            _ => false,
        },
        Value::Bool(value) => actual.as_bool().map(|v| v == *value).unwrap_or(false),
        Value::Null => actual.is_null(),
        Value::Array(arr) => {
            if arr.is_empty() {
                return actual.is_array()
                    && actual.as_array().map(|a| a.is_empty()).unwrap_or(false);
            }
            arr.iter().all(|entry| values_match(entry, actual))
        }
        Value::Object(_) => actual == expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_filters_query_on_string_items() {
        let args = serde_json::json!({ "query": "Evidence" });
        let filters = ListFilters::from_args(&args);
        let items = vec![
            serde_json::Value::String("evidence-2025-01.json".to_string()),
            serde_json::Value::String("artifact-foo.json".to_string()),
        ];
        let result = filters.apply(items, &[], None);
        assert_eq!(result.total, 1);
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].as_str(), Some("evidence-2025-01.json"));
    }

    #[test]
    fn list_filters_query_tags_where() {
        let items = vec![
            serde_json::json!({
                "name": "k8s.diff",
                "description": "Diff workloads",
                "tags": ["k8s", "read"],
                "tool": "ssh"
            }),
            serde_json::json!({
                "name": "db.backup",
                "description": "Backup",
                "tags": ["db"],
                "tool": "psql"
            }),
        ];
        let args = serde_json::json!({
            "query": "k8s",
            "tags": "read",
            "where": {"tool": "ssh"}
        });
        let filters = ListFilters::from_args(&args);
        let result = filters.apply(items, &["name", "description", "tool"], Some("tags"));
        assert_eq!(result.total, 1);
        assert_eq!(result.items.len(), 1);
        assert_eq!(
            result.items[0].get("name").and_then(|v| v.as_str()),
            Some("k8s.diff")
        );
    }

    #[test]
    fn list_filters_limit_offset() {
        let items = vec![
            serde_json::json!({"name": "a"}),
            serde_json::json!({"name": "b"}),
            serde_json::json!({"name": "c"}),
        ];
        let args = serde_json::json!({"limit": 1, "offset": 1});
        let filters = ListFilters::from_args(&args);
        let result = filters.apply(items, &["name"], None);
        assert_eq!(result.total, 3);
        assert_eq!(result.items.len(), 1);
        assert_eq!(
            result.items[0].get("name").and_then(|v| v.as_str()),
            Some("b")
        );
    }
}
