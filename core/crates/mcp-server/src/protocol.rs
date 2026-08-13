//! JSON-RPC 2.0 envelope types used by MCP.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const JSONRPC_VERSION: &str = "2.0";

/// MCP protocol revision this server implements.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

// Standard JSON-RPC error codes.
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Request {
    pub jsonrpc: String,
    /// Absent for notifications, which take no response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// Whether this is a notification — no `id`, therefore no reply.
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Response {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(ErrorObject {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_request_without_an_id_is_a_notification() {
        let notification: Request = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }))
        .unwrap();
        assert!(notification.is_notification());

        let call: Request = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
        }))
        .unwrap();
        assert!(!call.is_notification());
    }

    #[test]
    fn a_success_response_omits_the_error_field() {
        let json = serde_json::to_value(Response::success(json!(1), json!({"ok": true}))).unwrap();

        assert_eq!(json["jsonrpc"], "2.0");
        assert!(json.get("error").is_none(), "must not send a null error");
        assert_eq!(json["result"]["ok"], true);
    }

    #[test]
    fn an_error_response_omits_the_result_field() {
        let json =
            serde_json::to_value(Response::error(json!(1), METHOD_NOT_FOUND, "nope")).unwrap();

        assert!(json.get("result").is_none(), "must not send a null result");
        assert_eq!(json["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(json["error"]["message"], "nope");
    }

    #[test]
    fn string_ids_are_preserved() {
        // JSON-RPC ids may be strings or numbers; echoing the wrong type breaks clients.
        let response = Response::success(json!("req-abc"), json!({}));
        assert_eq!(response.id, json!("req-abc"));
    }

    #[test]
    fn requests_with_no_params_deserialize() {
        let request: Request =
            serde_json::from_value(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
                .unwrap();
        assert!(request.params.is_none());
    }
}
