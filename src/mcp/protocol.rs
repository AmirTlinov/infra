use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: Value, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_rpc_request_allows_missing_id_for_notifications() {
        let raw = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
        let parsed: JsonRpcRequest = serde_json::from_str(raw).expect("must parse");
        assert!(parsed.id.is_none());
        assert_eq!(parsed.method, "notifications/initialized");
    }

    #[test]
    fn json_rpc_request_parses_id_when_present() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
        let parsed: JsonRpcRequest = serde_json::from_str(raw).expect("must parse");
        assert!(parsed.id.is_some());
        assert_eq!(parsed.method, "tools/list");
    }
}
