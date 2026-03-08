use crate::errors::ToolError;
use crate::services::store_db::StoreDb;
use serde_json::Value;
use std::cmp::Reverse;

const NAMESPACE: &str = "operations";

#[derive(Clone)]
pub struct OperationService {
    store: StoreDb,
}

impl OperationService {
    pub fn new() -> Result<Self, ToolError> {
        Ok(Self {
            store: StoreDb::new()?,
        })
    }

    pub fn upsert(&self, operation_id: &str, value: &Value) -> Result<(), ToolError> {
        self.store
            .upsert(NAMESPACE, operation_id, value, Some("local"))
    }

    pub fn get(&self, operation_id: &str) -> Result<Option<Value>, ToolError> {
        Ok(self
            .store
            .get(NAMESPACE, operation_id)?
            .map(|record| record.value))
    }

    pub fn list(&self, limit: usize, status: Option<&str>) -> Result<Vec<Value>, ToolError> {
        let mut items = self
            .store
            .list(NAMESPACE)?
            .into_iter()
            .map(|record| record.value)
            .filter(|item| {
                status
                    .map(|status_filter| {
                        item.get("status").and_then(|v| v.as_str()) == Some(status_filter)
                    })
                    .unwrap_or(true)
            })
            .collect::<Vec<_>>();
        items.sort_by_key(|item| Reverse(timestamp_key(item)));
        items.truncate(limit);
        Ok(items)
    }
}

fn timestamp_key(value: &Value) -> i64 {
    for field in ["updated_at", "finished_at", "started_at", "created_at"] {
        if let Some(raw) = value.get(field).and_then(|v| v.as_str()) {
            if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(raw) {
                return parsed.timestamp_millis();
            }
        }
    }
    0
}
