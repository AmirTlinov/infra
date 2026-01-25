use crate::errors::ToolError;
use serde_json::Value;

pub fn normalize_identifier_part(value: &str) -> Result<String, ToolError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ToolError::invalid_params(
            "Identifier must be a non-empty string",
        ));
    }
    if trimmed.contains('\0') {
        return Err(ToolError::invalid_params(
            "Identifier must not contain null bytes",
        ));
    }
    let unquoted = if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    Ok(format!("\"{}\"", unquoted.replace('"', "\"\"")))
}

pub fn quote_qualified_identifier(identifier: &str) -> Result<String, ToolError> {
    let parts: Vec<&str> = identifier.split('.').collect();
    if parts.is_empty() {
        return Err(ToolError::invalid_params(
            "Identifier must be a non-empty string",
        ));
    }
    let mut out = Vec::new();
    for part in parts {
        out.push(normalize_identifier_part(part)?);
    }
    Ok(out.join("."))
}

pub fn normalize_table_context(
    table_name: &str,
    schema_name: Option<&str>,
) -> Result<serde_json::Value, ToolError> {
    if table_name.trim().is_empty() {
        return Err(ToolError::invalid_params("Table name is required"));
    }
    if let Some(schema) = schema_name {
        let qualified = format!(
            "{}.{}",
            normalize_identifier_part(schema)?,
            normalize_identifier_part(table_name)?
        );
        Ok(serde_json::json!({
            "schema": schema,
            "table": table_name,
            "qualified": qualified,
        }))
    } else {
        Ok(serde_json::json!({
            "schema": serde_json::Value::Null,
            "table": table_name,
            "qualified": quote_qualified_identifier(table_name)?,
        }))
    }
}

pub fn build_where_clause(
    filters: Option<&Value>,
    where_sql: Option<&str>,
    where_params: Option<&Vec<Value>>,
    start_index: i64,
) -> Result<(String, Vec<Value>, i64), ToolError> {
    if let Some(where_sql) = where_sql {
        let params = where_params.cloned().unwrap_or_default();
        return Ok((
            where_sql.to_string(),
            params.clone(),
            start_index + params.len() as i64,
        ));
    }

    if let Some(filters) = filters {
        return build_filters_clause(filters, start_index);
    }

    Ok((String::new(), Vec::new(), start_index))
}

fn build_filters_clause(
    filters: &Value,
    start_index: i64,
) -> Result<(String, Vec<Value>, i64), ToolError> {
    let mut clauses = Vec::new();
    let mut values = Vec::new();
    let mut index = start_index;

    let mut add_value = |value: Value| {
        values.push(value);
        let placeholder = format!("${}", index);
        index += 1;
        placeholder
    };

    let normalize_operator = |op: Option<&str>| op.unwrap_or("=").trim().to_uppercase();

    let mut push_filter = |column: &str,
                           op: Option<&str>,
                           value: &Value|
     -> Result<(), ToolError> {
        let column_sql = quote_qualified_identifier(column)?;
        let operator = normalize_operator(op);
        if value.is_null() {
            if operator == "!=" || operator == "<>" {
                clauses.push(format!("{} IS NOT NULL", column_sql));
            } else {
                clauses.push(format!("{} IS NULL", column_sql));
            }
            return Ok(());
        }
        if operator == "IN" || operator == "NOT IN" {
            let arr = value.as_array().ok_or_else(|| {
                ToolError::invalid_params(format!("{} filter requires a non-empty array", operator))
            })?;
            if arr.is_empty() {
                return Err(ToolError::invalid_params(format!(
                    "{} filter requires a non-empty array",
                    operator
                )));
            }
            let placeholders: Vec<String> =
                arr.iter().map(|entry| add_value(entry.clone())).collect();
            clauses.push(format!(
                "{} {} ({})",
                column_sql,
                operator,
                placeholders.join(", ")
            ));
            return Ok(());
        }
        let placeholder = add_value(value.clone());
        clauses.push(format!("{} {} {}", column_sql, operator, placeholder));
        Ok(())
    };

    if let Some(array) = filters.as_array() {
        for item in array {
            let obj = item
                .as_object()
                .ok_or_else(|| ToolError::invalid_params("Filter item must be an object"))?;
            let column = obj
                .get("column")
                .or_else(|| obj.get("field"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::invalid_params("Filter item must include column"))?;
            let op = obj.get("op").and_then(|v| v.as_str());
            let value = obj.get("value").unwrap_or(&Value::Null);
            push_filter(column, op, value)?;
        }
    } else if let Some(map) = filters.as_object() {
        for (column, value) in map.iter() {
            push_filter(column, Some("="), value)?;
        }
    }

    Ok((clauses.join(" AND "), values, index))
}
