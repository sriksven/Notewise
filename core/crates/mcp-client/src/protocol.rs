//! JSON-RPC 2.0 framing, and the three MCP methods this client speaks.
//!
//! # Written twice on purpose
//!
//! `mcp-server/src/protocol.rs` has the same envelope types. `CLAUDE.md` rule 2 says surfaces never
//! depend on each other, and `mcp-server` is a surface — so the duplication is accepted rather than
//! resolved by extracting a shared crate mid-spec. The design doc records it as the obvious
//! follow-up.
//!
//! The two are not quite mirror images anyway: that one *answers* requests and so must tolerate
//! anything a client sends, while this one *makes* them and must tolerate anything a server
//! replies. The tolerances point in opposite directions.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{McpError, Result, ToolDef};

pub const JSONRPC_VERSION: &str = "2.0";

/// MCP revision this client asks for.
///
/// Kept equal to `mcp-server`'s so a Notewise-to-Notewise connection cannot fail a version check
/// against itself.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// A request or a notification, on the way out.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Outgoing {
    pub jsonrpc: &'static str,
    /// Absent for a notification, which takes no reply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Outgoing {
    pub fn request(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id: Some(id),
            method: method.to_string(),
            params: params_of(params),
        }
    }

    pub fn notification(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id: None,
            method: method.to_string(),
            params: params_of(params),
        }
    }
}

/// `null` params are omitted rather than sent: some servers validate the field's type and reject
/// an explicit null, which reads as a protocol error for a call that was actually fine.
fn params_of(params: Value) -> Option<Value> {
    if params.is_null() {
        None
    } else {
        Some(params)
    }
}

/// Anything a server sent us.
///
/// Every field is optional because this is what arrives from someone else's implementation. A
/// server-initiated notification has no `id`, a response has no `method`, and both turn up on the
/// same pipe — see [`Incoming::is_reply_to`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Incoming {
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<ErrorObject>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

impl Incoming {
    /// Whether this is the reply we are waiting for.
    ///
    /// Servers interleave `notifications/message` log lines with replies, so a read loop that
    /// took the first line it saw would return a log entry as a tool result.
    pub fn is_reply_to(&self, id: u64) -> bool {
        self.id.as_ref().and_then(Value::as_u64) == Some(id) && self.method.is_none()
    }

    /// The result, or the server's error turned into ours.
    pub fn into_result(self, server: &str) -> Result<Value> {
        if let Some(error) = self.error {
            let detail = match error.data {
                Some(data) => format!("{} ({}): {data}", error.message, error.code),
                None => format!("{} ({})", error.message, error.code),
            };
            return Err(McpError::Rpc {
                server: server.to_string(),
                detail,
            });
        }
        // A reply with neither result nor error is malformed. Reported rather than defaulted to
        // an empty object, which would read as a call that succeeded and did nothing.
        self.result.ok_or_else(|| McpError::Transport {
            server: server.to_string(),
            detail: "a reply carried neither a result nor an error".into(),
        })
    }
}

/// What we tell a server about ourselves during `initialize`.
pub(crate) fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        // No capabilities are declared, and that is the whole story: this client does not accept
        // sampling requests, does not expose roots, and does not subscribe to anything. A server
        // that reads this knows not to ask.
        "capabilities": {},
        "clientInfo": {
            "name": "notewise",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

/// Check the server's half of the handshake.
///
/// A version mismatch is reported rather than ignored. Proceeding against a revision we do not
/// implement means a tool call whose arguments mean something different than we think.
pub(crate) fn check_initialize(server: &str, result: &Value) -> Result<String> {
    let version = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::Handshake {
            server: server.to_string(),
            detail: "the server's initialize reply had no protocolVersion".into(),
        })?;

    if version != PROTOCOL_VERSION {
        return Err(McpError::Handshake {
            server: server.to_string(),
            detail: format!(
                "the server speaks MCP {version}; this build speaks {PROTOCOL_VERSION}"
            ),
        });
    }

    Ok(result
        .get("serverInfo")
        .and_then(|info| info.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(server)
        .to_string())
}

/// Read a `tools/list` reply.
///
/// A tool whose entry cannot be decoded is dropped rather than failing the whole listing: one
/// malformed tool must not hide the other nine, and a tool that is not listed simply cannot be
/// proposed — which is the safe direction.
pub(crate) fn parse_tools(result: &Value) -> Vec<ToolDef> {
    let Some(tools) = result.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };

    tools
        .iter()
        .filter_map(
            |entry| match serde_json::from_value::<ToolDef>(entry.clone()) {
                Ok(tool) => Some(tool),
                Err(e) => {
                    tracing::warn!(error = %e, "skipping a tool entry this build cannot read");
                    None
                }
            },
        )
        .collect()
}

/// Read a `tools/call` reply.
///
/// # `isError` is the trap here
///
/// MCP reports a *tool* failure inside a *successful* JSON-RPC response. A client that only checked
/// the envelope would record "the ticket was filed" for a reply that says the ticket was rejected,
/// and the user would never look again. So the flag is checked, and a set flag becomes
/// [`McpError::ToolError`].
pub(crate) fn parse_tool_result(tool: &str, result: Value) -> Result<Value> {
    let failed = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if failed {
        return Err(McpError::ToolError {
            tool: tool.to_string(),
            detail: text_of(&result).unwrap_or_else(|| "the tool reported an error".into()),
        });
    }

    Ok(result)
}

/// Flatten the `content` array's text parts into one string, for showing a human.
///
/// Non-text content — an image, an embedded resource — is described rather than rendered, because
/// the alternative is a base64 blob in a results panel.
pub fn text_of(result: &Value) -> Option<String> {
    let content = result.get("content").and_then(Value::as_array)?;

    let parts: Vec<String> = content
        .iter()
        .map(|part| match part.get("type").and_then(Value::as_str) {
            Some("text") => part
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            Some(other) => format!("[{other}]"),
            None => String::new(),
        })
        .filter(|part| !part.is_empty())
        .collect();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notification_carries_no_id() {
        let json = serde_json::to_value(Outgoing::notification(
            "notifications/initialized",
            Value::Null,
        ))
        .expect("serializes");

        assert!(json.get("id").is_none(), "a notification takes no reply");
        assert!(
            json.get("params").is_none(),
            "null params are omitted: some servers reject an explicit null"
        );
    }

    #[test]
    fn a_request_carries_its_id() {
        let json = serde_json::to_value(Outgoing::request(7, "tools/list", Value::Null))
            .expect("serializes");
        assert_eq!(json["id"], 7);
        assert_eq!(json["jsonrpc"], "2.0");
    }

    /// A log line arriving between the request and its reply must not be mistaken for the reply.
    #[test]
    fn a_server_notification_is_not_a_reply() {
        let log: Incoming = serde_json::from_str(
            r#"{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info"}}"#,
        )
        .expect("parses");
        assert!(!log.is_reply_to(1));

        let reply: Incoming =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#).expect("parses");
        assert!(reply.is_reply_to(1));
        assert!(!reply.is_reply_to(2), "another call's reply is not ours");
    }

    #[test]
    fn a_server_error_becomes_our_error_with_its_detail() {
        let incoming: Incoming = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no such method"}}"#,
        )
        .expect("parses");

        let err = incoming.into_result("linear").expect_err("must fail");
        let rendered = err.to_string();
        assert!(rendered.contains("no such method"), "{rendered}");
        assert!(rendered.contains("-32601"), "{rendered}");
    }

    /// Defaulting this to an empty object would read as a call that succeeded and did nothing.
    #[test]
    fn a_reply_with_neither_result_nor_error_is_a_transport_fault() {
        let incoming: Incoming =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1}"#).expect("parses");
        assert!(matches!(
            incoming.into_result("linear"),
            Err(McpError::Transport { .. })
        ));
    }

    #[test]
    fn a_matching_protocol_version_names_the_server() {
        let name = check_initialize(
            "linear",
            &serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "serverInfo": { "name": "linear-mcp", "version": "1.2.3" }
            }),
        )
        .expect("handshake succeeds");
        assert_eq!(name, "linear-mcp");
    }

    /// Proceeding against a revision we do not implement means arguments that mean something else.
    #[test]
    fn a_different_protocol_version_fails_the_handshake() {
        let err = check_initialize(
            "linear",
            &serde_json::json!({ "protocolVersion": "1999-01-01" }),
        )
        .expect_err("must refuse");

        let rendered = err.to_string();
        assert!(rendered.contains("1999-01-01"), "{rendered}");
        assert!(rendered.contains(PROTOCOL_VERSION), "{rendered}");
    }

    #[test]
    fn a_handshake_reply_without_a_version_is_refused() {
        assert!(check_initialize("linear", &serde_json::json!({})).is_err());
    }

    #[test]
    fn tools_are_read_with_their_schemas() {
        let tools = parse_tools(&serde_json::json!({
            "tools": [{
                "name": "create_issue",
                "description": "File one",
                "inputSchema": { "type": "object", "properties": { "title": { "type": "string" } } }
            }]
        }));

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "create_issue");
        assert_eq!(
            tools[0].input_schema["properties"]["title"]["type"],
            "string"
        );
    }

    /// One malformed tool must not hide the other nine.
    #[test]
    fn a_malformed_tool_entry_is_skipped_rather_than_fatal() {
        let tools = parse_tools(&serde_json::json!({
            "tools": [
                { "no_name_field": true },
                { "name": "usable" }
            ]
        }));

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "usable");
    }

    #[test]
    fn a_listing_with_no_tools_is_empty_rather_than_an_error() {
        assert!(parse_tools(&serde_json::json!({})).is_empty());
    }

    /// The trap: MCP reports a tool failure inside a successful JSON-RPC response.
    #[test]
    fn a_tool_that_reports_its_own_failure_is_not_a_success() {
        let err = parse_tool_result(
            "create_issue",
            serde_json::json!({
                "isError": true,
                "content": [{ "type": "text", "text": "project not found" }]
            }),
        )
        .expect_err("must not be recorded as done");

        match err {
            McpError::ToolError { tool, detail } => {
                assert_eq!(tool, "create_issue");
                assert!(detail.contains("project not found"), "{detail}");
            }
            other => panic!("expected a tool error, got {other:?}"),
        }
    }

    #[test]
    fn a_successful_tool_result_passes_through_whole() {
        let result = parse_tool_result(
            "create_issue",
            serde_json::json!({ "content": [{ "type": "text", "text": "ENG-421" }] }),
        )
        .expect("succeeds");
        assert_eq!(text_of(&result).as_deref(), Some("ENG-421"));
    }

    #[test]
    fn several_text_parts_are_joined() {
        let result = serde_json::json!({
            "content": [
                { "type": "text", "text": "first" },
                { "type": "text", "text": "second" }
            ]
        });
        assert_eq!(text_of(&result).as_deref(), Some("first\nsecond"));
    }

    /// The alternative is a base64 blob in a results panel.
    #[test]
    fn non_text_content_is_described_rather_than_rendered() {
        let result = serde_json::json!({
            "content": [{ "type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png" }]
        });
        let text = text_of(&result).expect("describes it");
        assert_eq!(text, "[image]");
        assert!(!text.contains("iVBORw0KGgo"), "{text}");
    }

    #[test]
    fn a_result_with_no_content_has_no_text() {
        assert_eq!(text_of(&serde_json::json!({})), None);
    }

    #[test]
    fn the_client_declares_no_capabilities() {
        let params = initialize_params();
        assert_eq!(params["capabilities"], serde_json::json!({}));
        assert_eq!(params["clientInfo"]["name"], "notewise");
        assert_eq!(params["protocolVersion"], PROTOCOL_VERSION);
    }
}
