use serde_json::Value;

pub fn manifest_ref(entry: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in [
        "name",
        "source",
        "manifest_source",
        "manifest_path",
        "manifest_version",
        "manifest_sha256",
    ] {
        if let Some(value) = entry.get(key) {
            if !value.is_null() {
                out.insert(key.to_string(), value.clone());
            }
        }
    }

    if out.is_empty() {
        Value::Null
    } else {
        Value::Object(out)
    }
}
