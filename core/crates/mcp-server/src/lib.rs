//! MCP server exposing the Notewise workspace to agents.
//!
//! Speaks JSON-RPC 2.0 over stdio, the transport MCP clients (Claude Code, Cursor, and
//! others) launch a server with. Every tool is **read-only** — see [`tools`] for why.
//!
//! # Example
//!
//! ```
//! use notewise_mcp_server::McpServer;
//! use notewise_storage::Database;
//! use serde_json::json;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let server = McpServer::new(Database::open_in_memory()?);
//!
//! let response = server.handle_value(json!({
//!     "jsonrpc": "2.0",
//!     "id": 1,
//!     "method": "tools/list",
//! })).expect("tools/list returns a response");
//!
//! assert!(response["result"]["tools"].as_array().unwrap().len() >= 6);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod error;
mod protocol;
mod tools;

pub use error::{McpError, Result};
pub use protocol::PROTOCOL_VERSION;

use serde_json::{json, Value};

use notewise_storage::Database;

use crate::protocol::{Request, Response, INVALID_REQUEST, JSONRPC_VERSION, PARSE_ERROR};

/// An MCP server over one local database.
#[derive(Debug)]
pub struct McpServer {
    db: Database,
}

impl McpServer {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Handle one line of JSON-RPC input.
    ///
    /// Returns `None` for notifications, which take no reply. A parse failure produces a
    /// well-formed error response rather than propagating — a malformed line from a client
    /// must not take the server down mid-session.
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(e) => {
                return Some(
                    serde_json::to_string(&Response::error(
                        Value::Null,
                        PARSE_ERROR,
                        format!("invalid JSON: {e}"),
                    ))
                    .expect("error response always serializes"),
                )
            }
        };

        self.handle_value(value)
            .map(|response| serde_json::to_string(&response).expect("response always serializes"))
    }

    /// Handle one parsed request. Returns `None` for notifications.
    pub fn handle_value(&self, value: Value) -> Option<Value> {
        let request: Request = match serde_json::from_value(value) {
            Ok(request) => request,
            Err(e) => {
                return Some(
                    serde_json::to_value(Response::error(
                        Value::Null,
                        INVALID_REQUEST,
                        format!("not a valid JSON-RPC request: {e}"),
                    ))
                    .expect("error response always serializes"),
                )
            }
        };

        if request.jsonrpc != JSONRPC_VERSION {
            return Some(
                serde_json::to_value(Response::error(
                    request.id.unwrap_or(Value::Null),
                    INVALID_REQUEST,
                    format!("unsupported jsonrpc version '{}'", request.jsonrpc),
                ))
                .expect("error response always serializes"),
            );
        }

        // Notifications get no reply, per JSON-RPC.
        if request.is_notification() {
            tracing::debug!(method = %request.method, "notification");
            return None;
        }

        let id = request.id.clone().expect("checked above");
        let response = match self.dispatch(&request) {
            Ok(result) => Response::success(id, result),
            Err(e) => Response::error(id, e.code(), e.to_string()),
        };

        Some(serde_json::to_value(response).expect("response always serializes"))
    }

    fn dispatch(&self, request: &Request) -> Result<Value> {
        match request.method.as_str() {
            "initialize" => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "notewise",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            })),

            "tools/list" => Ok(json!({ "tools": tools::definitions() })),

            "tools/call" => {
                let params = request.params.as_ref().ok_or_else(|| {
                    McpError::InvalidParams("tools/call requires params".into())
                })?;

                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| McpError::InvalidParams("'name' is required".into()))?;

                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let result = tools::call(&self.db, name, &args)?;

                // MCP wraps tool output in a content array. JSON is delivered as text
                // because that is what the content schema carries.
                Ok(json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&result)
                            .expect("tool output always serializes"),
                    }],
                    "isError": false,
                }))
            }

            "ping" => Ok(json!({})),

            other => Err(McpError::UnsupportedMethod(other.to_string())),
        }
    }

    /// Serve JSON-RPC over stdin/stdout until stdin closes.
    pub async fn serve_stdio(self) -> std::io::Result<()> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let stdin = BufReader::new(tokio::io::stdin());
        let mut stdout = tokio::io::stdout();
        let mut lines = stdin.lines();

        tracing::info!("notewise mcp server ready on stdio");

        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(response) = self.handle_line(&line) {
                stdout.write_all(response.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> McpServer {
        McpServer::new(Database::open_in_memory().expect("in-memory db"))
    }

    fn call(server: &McpServer, method: &str, params: Value) -> Value {
        server
            .handle_value(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params,
            }))
            .expect("a request with an id gets a response")
    }

    #[test]
    fn initialize_advertises_tool_support() {
        let response = call(&server(), "initialize", json!({}));

        assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(response["result"]["capabilities"]["tools"].is_object());
        assert_eq!(response["result"]["serverInfo"]["name"], "notewise");
    }

    #[test]
    fn tools_list_returns_the_declared_tools() {
        let response = call(&server(), "tools/list", json!({}));
        let tools = response["result"]["tools"].as_array().unwrap();

        assert!(tools.len() >= 6);
        assert!(tools.iter().any(|t| t["name"] == "search"));
        assert!(tools.iter().any(|t| t["name"] == "find_related"));
    }

    #[test]
    fn a_tool_call_returns_mcp_content() {
        let response = call(
            &server(),
            "tools/call",
            json!({ "name": "list_meetings", "arguments": {} }),
        );

        let content = &response["result"]["content"][0];
        assert_eq!(content["type"], "text");
        assert_eq!(response["result"]["isError"], false);

        // The text payload is itself JSON the agent can parse.
        let parsed: Value = serde_json::from_str(content["text"].as_str().unwrap()).unwrap();
        assert!(parsed["meetings"].is_array());
    }

    #[test]
    fn notifications_get_no_reply() {
        let response = server().handle_value(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }));
        assert!(response.is_none(), "notifications must not be answered");
    }

    #[test]
    fn an_unknown_method_is_method_not_found() {
        let response = call(&server(), "does/not/exist", json!({}));
        assert_eq!(response["error"]["code"], protocol::METHOD_NOT_FOUND);
    }

    #[test]
    fn an_unknown_tool_is_method_not_found() {
        let response = call(
            &server(),
            "tools/call",
            json!({ "name": "drop_database", "arguments": {} }),
        );
        assert_eq!(response["error"]["code"], protocol::METHOD_NOT_FOUND);
    }

    #[test]
    fn a_tool_call_without_a_name_is_invalid_params() {
        let response = call(&server(), "tools/call", json!({ "arguments": {} }));
        assert_eq!(response["error"]["code"], protocol::INVALID_PARAMS);
    }

    #[test]
    fn malformed_json_produces_a_parse_error_rather_than_a_crash() {
        let raw = server().handle_line("{not json").expect("should reply");
        let response: Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(response["error"]["code"], protocol::PARSE_ERROR);
        assert_eq!(response["id"], Value::Null);
    }

    #[test]
    fn a_wrong_jsonrpc_version_is_rejected() {
        let response = server()
            .handle_value(json!({ "jsonrpc": "1.0", "id": 1, "method": "ping" }))
            .expect("should reply");
        assert_eq!(response["error"]["code"], protocol::INVALID_REQUEST);
    }

    #[test]
    fn the_request_id_is_echoed_with_its_original_type() {
        for id in [json!(42), json!("req-abc")] {
            let response = server()
                .handle_value(json!({ "jsonrpc": "2.0", "id": id, "method": "ping" }))
                .expect("should reply");
            assert_eq!(response["id"], id);
        }
    }

    #[test]
    fn responses_never_carry_both_a_result_and_an_error() {
        let ok = call(&server(), "ping", json!({}));
        assert!(ok.get("error").is_none());
        assert!(ok.get("result").is_some());

        let err = call(&server(), "nope", json!({}));
        assert!(err.get("result").is_none());
        assert!(err.get("error").is_some());
    }

    #[test]
    fn handle_line_round_trips_through_strings() {
        let raw = server()
            .handle_line(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#)
            .expect("should reply");
        let response: Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(response["id"], 7);
        assert!(response["result"]["tools"].is_array());
    }
}
