use crate::constants::limits::{MAX_PORT, MIN_PORT};
use crate::errors::ToolError;
use serde_json::Value;

#[derive(Clone)]
pub struct Validation;

impl Validation {
    pub fn new() -> Self {
        Self
    }

    pub fn ensure_string(
        &self,
        value: &Value,
        label: &str,
        trim: bool,
    ) -> Result<String, ToolError> {
        let text = value.as_str().ok_or_else(|| {
            ToolError::invalid_params(format!("{} must be a non-empty string", label))
        })?;
        let normalized = text.trim();
        if normalized.is_empty() {
            return Err(ToolError::invalid_params(format!(
                "{} must be a non-empty string",
                label
            )));
        }
        Ok(if trim {
            normalized.to_string()
        } else {
            text.to_string()
        })
    }

    pub fn ensure_optional_string(
        &self,
        value: Option<&Value>,
        label: &str,
        trim: bool,
    ) -> Result<Option<String>, ToolError> {
        match value {
            None => Ok(None),
            Some(val) if val.is_null() => Ok(None),
            Some(val) => self.ensure_string(val, label, trim).map(Some),
        }
    }

    pub fn ensure_port(
        &self,
        value: Option<&Value>,
        fallback: Option<u16>,
    ) -> Result<u16, ToolError> {
        let Some(value) = value else {
            return Ok(fallback.unwrap_or(MIN_PORT));
        };
        if value.is_null() {
            return Ok(fallback.unwrap_or(MIN_PORT));
        }
        let numeric = value
            .as_i64()
            .or_else(|| value.as_str().and_then(|s| s.parse::<i64>().ok()))
            .ok_or_else(|| {
                ToolError::invalid_params(format!(
                    "Port must be an integer between {} and {}",
                    MIN_PORT, MAX_PORT
                ))
            })?;
        if numeric < MIN_PORT as i64 || numeric > MAX_PORT as i64 {
            return Err(ToolError::invalid_params(format!(
                "Port must be an integer between {} and {}",
                MIN_PORT, MAX_PORT
            )));
        }
        Ok(numeric as u16)
    }

    pub fn ensure_identifier(&self, value: &str, label: &str) -> Result<String, ToolError> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(ToolError::invalid_params(format!(
                "{} must be a non-empty string",
                label
            )));
        }
        if trimmed.contains('\0') {
            return Err(ToolError::invalid_params(format!(
                "{} must not contain null bytes",
                label
            )));
        }
        Ok(trimmed.to_string())
    }

    pub fn ensure_table_name(&self, value: &str) -> Result<String, ToolError> {
        self.ensure_identifier(value, "Table name")
    }

    pub fn ensure_schema_name(&self, value: &str) -> Result<String, ToolError> {
        self.ensure_identifier(value, "Schema name")
    }

    pub fn ensure_data_object(
        &self,
        value: &Value,
    ) -> Result<serde_json::Map<String, Value>, ToolError> {
        let obj = value
            .as_object()
            .ok_or_else(|| ToolError::invalid_params("Data must be an object"))?;
        if obj.is_empty() {
            return Err(ToolError::invalid_params("Data object must not be empty"));
        }
        Ok(obj.clone())
    }

    pub fn ensure_headers(
        &self,
        value: Option<&Value>,
    ) -> Result<serde_json::Map<String, Value>, ToolError> {
        let Some(value) = value else {
            return Ok(Default::default());
        };
        if value.is_null() {
            return Ok(Default::default());
        }
        let obj = value
            .as_object()
            .ok_or_else(|| ToolError::invalid_params("Headers must be an object"))?;
        let mut out = serde_json::Map::new();
        for (key, val) in obj.iter() {
            if key.trim().is_empty() {
                continue;
            }
            if val.is_null() {
                continue;
            }
            let rendered = val
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| val.to_string());
            out.insert(key.trim().to_string(), Value::String(rendered));
        }
        Ok(out)
    }

    pub fn ensure_object(
        &self,
        value: &Value,
        label: &str,
    ) -> Result<serde_json::Map<String, Value>, ToolError> {
        value
            .as_object()
            .cloned()
            .ok_or_else(|| ToolError::invalid_params(format!("{} must be an object", label)))
    }

    pub fn ensure_optional_object(
        &self,
        value: Option<&Value>,
        label: &str,
    ) -> Result<Option<serde_json::Map<String, Value>>, ToolError> {
        match value {
            None => Ok(None),
            Some(val) if val.is_null() => Ok(None),
            Some(val) => self.ensure_object(val, label).map(Some),
        }
    }
}

impl Default for Validation {
    fn default() -> Self {
        Self::new()
    }
}
