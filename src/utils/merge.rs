use serde_json::{Map, Value};

pub fn is_plain_object(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

pub fn merge_deep(base: &Value, override_value: &Value) -> Value {
    if !is_plain_object(base) || !is_plain_object(override_value) {
        if !override_value.is_null() {
            return override_value.clone();
        }
        return base.clone();
    }

    let mut result = match base {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };

    if let Value::Object(override_map) = override_value {
        for (key, value) in override_map.iter() {
            if value.is_null() {
                result.insert(key.clone(), Value::Null);
                continue;
            }
            let existing = result.get(key).cloned().unwrap_or(Value::Null);
            if is_plain_object(value) && is_plain_object(&existing) {
                result.insert(key.clone(), merge_deep(&existing, value));
            } else {
                result.insert(key.clone(), value.clone());
            }
        }
    }

    Value::Object(result)
}
