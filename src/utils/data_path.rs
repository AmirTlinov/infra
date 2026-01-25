use crate::errors::ToolError;
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum PathSegment {
    Key(String),
    Index(usize),
}

pub fn parse_path(path: &str) -> Result<Vec<PathSegment>, ToolError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ToolError::invalid_params("Path must be a non-empty string"));
    }
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_brackets = false;
    for ch in trimmed.chars() {
        match ch {
            '.' if !in_brackets => {
                if !current.trim().is_empty() {
                    segments.push(segment_from(&current));
                }
                current.clear();
            }
            '[' => {
                if !current.trim().is_empty() {
                    segments.push(segment_from(&current));
                    current.clear();
                }
                in_brackets = true;
            }
            ']' => {
                if !current.trim().is_empty() {
                    segments.push(segment_from(&current));
                }
                current.clear();
                in_brackets = false;
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        segments.push(segment_from(&current));
    }
    Ok(segments)
}

fn segment_from(raw: &str) -> PathSegment {
    let trimmed = raw
        .trim()
        .trim_matches('"')
        .trim_matches('"')
        .trim_matches('\'')
        .trim();
    if let Ok(index) = trimmed.parse::<usize>() {
        return PathSegment::Index(index);
    }
    PathSegment::Key(trimmed.to_string())
}

pub fn get_path_value(
    target: &Value,
    path: &str,
    required: bool,
    default_value: Option<Value>,
) -> Result<Value, ToolError> {
    if path.trim().is_empty() {
        return Ok(target.clone());
    }
    let segments = parse_path(path)?;
    let mut current = target;
    for segment in segments.iter() {
        match segment {
            PathSegment::Key(key) => match current.get(key) {
                Some(value) => current = value,
                None => {
                    return if required {
                        Err(missing(path))
                    } else {
                        Ok(default_value.unwrap_or(Value::Null))
                    };
                }
            },
            PathSegment::Index(index) => match current.as_array().and_then(|arr| arr.get(*index)) {
                Some(value) => current = value,
                None => {
                    return if required {
                        Err(missing(path))
                    } else {
                        Ok(default_value.unwrap_or(Value::Null))
                    };
                }
            },
        }
    }
    Ok(current.clone())
}

fn missing(path: &str) -> ToolError {
    ToolError::invalid_params(format!("Path '{}' not found", path))
}
