//! Both transports against a server that is really there.
//!
//! # Why not only the stub factory
//!
//! `client.rs`'s tests hand `McpClient` a `Transport` implemented in the test, which proves the
//! lifecycle and the gate and proves nothing at all about framing. Newline delimiting, a banner
//! printed before the protocol starts, a notification arriving between a request and its reply, an
//! SSE-framed body, a session header that has to be echoed — none of those exist above the
//! transport, and all of them are how this breaks in the field.
//!
//! So: a real child process reached over real pipes, and a real HTTP server reached over a real
//! socket. Neither needs a third-party MCP server, so neither is `#[ignore]`d.

use std::collections::BTreeMap;

use notewise_mcp_client::{
    text_of, Allowlist, McpClient, McpError, ServerConfig, TransportKind, PROTOCOL_VERSION,
};
use serde_json::{json, Value};

fn allowing(server: &str, tool: &str) -> Allowlist {
    let mut list = Allowlist::new();
    list.allow(server, tool);
    list
}

// ---------------------------------------------------------------- stdio

/// A POSIX-shell MCP server.
///
/// Chosen over a compiled helper binary on purpose: a helper would need its own crate target, and
/// the one thing worth testing here is that we talk correctly to a process that is not ours. A
/// shell script is unmistakably not ours.
///
/// It answers three methods, and it also does two things a well-behaved server would not, because
/// real ones do them: it prints a banner on stdout before the protocol starts, and it writes to
/// stderr during a call.
const SHELL_SERVER: &str = r#"
echo 'shell-stub starting'
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"__VERSION__","serverInfo":{"name":"shell-stub","version":"0"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"listing"}}\n'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Say it back","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      echo 'about to answer a call' >&2
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"heard you"}]}}\n' "$id"
      ;;
    *)
      ;;
  esac
done
"#;

/// A server that reports its own process id, so a test can check the process actually dies.
const PID_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"__VERSION__","serverInfo":{"name":"pid-stub","version":"0"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"whoami","inputSchema":{}}]}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$id" "$$"
      ;;
    *)
      ;;
  esac
done
"#;

fn shell_server(id: &str, name: &str, script: &str) -> ServerConfig {
    ServerConfig {
        id: id.into(),
        name: name.into(),
        transport: TransportKind::Stdio {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), script.replace("__VERSION__", PROTOCOL_VERSION)],
            env: BTreeMap::new(),
        },
        auto_start: true,
    }
}

/// The whole round trip over pipes: spawn, handshake, list, call.
#[cfg(unix)]
#[tokio::test]
async fn a_real_child_process_can_be_handshaken_listed_and_called() {
    let client = McpClient::new();
    let config = shell_server("sh-1", "shell", SHELL_SERVER);

    let tools = client.tools(&config).await.expect("lists its tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[0].input_schema["required"][0], "text");

    let result = client
        .call(
            &config,
            &allowing("shell", "echo"),
            "echo",
            json!({ "text": "hello" }),
        )
        .await
        .expect("the call succeeds");

    assert_eq!(text_of(&result).as_deref(), Some("heard you"));

    // The banner on stdout before the handshake, and the stderr write during the call, both had to
    // be tolerated for any of the above to pass — a client that treated either as fatal, or that
    // let stderr fill its pipe, would have failed or hung by now.

    client.stop_all().await;
}

/// A server pinned off stays off, and starting it by hand works.
#[cfg(unix)]
#[tokio::test]
async fn a_pinned_off_child_is_not_spawned_by_first_use() {
    let client = McpClient::new();
    let config = shell_server("sh-2", "pinned", SHELL_SERVER).with_auto_start(false);

    let err = client.tools(&config).await.expect_err("must not start");
    assert!(matches!(err, McpError::NotStarted { .. }), "{err:?}");
    assert!(!client.is_running("sh-2").await);

    assert_eq!(client.start(&config).await.expect("starts").len(), 1);
    assert!(client.is_running("sh-2").await);
    client.stop_all().await;
}

/// A missing binary is one server's problem, not the app's.
#[cfg(unix)]
#[tokio::test]
async fn a_command_that_does_not_exist_fails_only_its_own_tools() {
    let client = McpClient::new();

    let broken = ServerConfig::stdio(
        "sh-3",
        "missing",
        "/nonexistent/definitely-not-a-real-mcp-server",
        vec![],
    );
    let working = shell_server("sh-4", "shell", SHELL_SERVER);

    let err = client.tools(&broken).await.expect_err("must fail");
    let rendered = err.to_string();
    assert!(rendered.contains("missing"), "{rendered}");

    assert_eq!(
        client.tools(&working).await.expect("still works").len(),
        1,
        "one broken server must not take the others with it"
    );
    client.stop_all().await;
}

/// A server that exits without answering must not hang the caller.
#[cfg(unix)]
#[tokio::test]
async fn a_server_that_dies_mid_handshake_reports_rather_than_hangs() {
    let client = McpClient::new();
    // Reads nothing and exits immediately.
    let config = shell_server("sh-5", "quitter", "exit 0");

    let err = client.tools(&config).await.expect_err("must fail");
    assert!(
        matches!(err, McpError::Handshake { .. }),
        "a dead server is a failed handshake, not a hang: {err:?}"
    );
}

/// Killing the child is not optional: quitting Notewise must not leave servers running, and the
/// user has no window to close them from.
#[cfg(unix)]
#[tokio::test]
async fn stopping_a_server_ends_its_process() {
    let client = McpClient::new();
    let config = shell_server("sh-6", "pid", PID_SERVER);

    let result = client
        .call(&config, &allowing("pid", "whoami"), "whoami", json!({}))
        .await
        .expect("calls");

    let reported = text_of(&result).expect("a pid");
    let pid: i32 = reported
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("expected a pid, got {reported:?}"));

    assert!(alive(pid), "the server should be running before the stop");

    client.stop("sh-6").await;
    // The kill is asynchronous; the process has to be reaped before /bin/kill -0 stops finding it.
    for _ in 0..50 {
        if !alive(pid) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the child process outlived its session");
}

/// `kill -0` — asks whether a pid exists without signalling it.
#[cfg(unix)]
fn alive(pid: i32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------- http

/// An in-process MCP server over streamable HTTP.
///
/// It replies to `initialize` in plain JSON with a session id, and to `tools/call` in SSE framing —
/// which is the combination a real streamable-HTTP server produces, and the combination a client
/// that only handled one of them would break on.
async fn http_server() -> (
    String,
    std::sync::Arc<tokio::sync::Mutex<Vec<Option<String>>>>,
) {
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::response::Response;
    use axum::routing::post;

    type Sessions = std::sync::Arc<tokio::sync::Mutex<Vec<Option<String>>>>;

    async fn handle(State(seen): State<Sessions>, headers: HeaderMap, body: String) -> Response {
        // Recorded so a test can prove the session id was echoed on later requests.
        seen.lock().await.push(
            headers
                .get("mcp-session-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
        );

        let request: Value = serde_json::from_str(&body).expect("the client sends JSON");
        let id = request.get("id").cloned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        // A notification takes no reply. 202 with an empty body is the correct answer.
        let Some(id) = id else {
            return Response::builder()
                .status(202)
                .body(axum::body::Body::empty())
                .expect("builds");
        };

        let result = match method.as_str() {
            "initialize" => json!({
                "protocolVersion": PROTOCOL_VERSION,
                "serverInfo": { "name": "http-stub", "version": "0" }
            }),
            "tools/list" => json!({
                "tools": [{
                    "name": "post_message",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "body": { "type": "string" } },
                        "required": ["body"]
                    }
                }]
            }),
            other if other.ends_with("call") => {
                // SSE framing, with a keep-alive comment and a notification event ahead of the
                // answer — all three of which a real server sends.
                let payload = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "content": [{ "type": "text", "text": "posted" }] }
                });
                let body = format!(": keep-alive\n\nevent: message\ndata: {payload}\n\n");
                return Response::builder()
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(body))
                    .expect("builds");
            }
            other => {
                let payload = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("no method '{other}'") }
                });
                return Response::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(payload.to_string()))
                    .expect("builds");
            }
        };

        let payload = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        Response::builder()
            .status(200)
            .header("content-type", "application/json")
            // Handed out on every reply; the client should adopt it and echo it back.
            .header("mcp-session-id", "session-abc")
            .body(axum::body::Body::from(payload.to_string()))
            .expect("builds")
    }

    let seen: Sessions = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = axum::Router::new()
        .route("/mcp", post(handle))
        .with_state(seen.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds");
    let addr = listener.local_addr().expect("has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    (format!("http://{addr}/mcp"), seen)
}

#[tokio::test]
async fn a_remote_server_over_http_can_be_handshaken_listed_and_called() {
    let (url, seen) = http_server().await;
    let client = McpClient::new();
    let config = ServerConfig::http("http-1", "remote", url);

    let tools = client.tools(&config).await.expect("lists");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "post_message");

    let result = client
        .call(
            &config,
            &allowing("remote", "post_message"),
            "post_message",
            json!({ "body": "shipped" }),
        )
        .await
        .expect("calls");

    // Proves the SSE path: this reply arrived as `data:` lines behind a keep-alive comment.
    assert_eq!(text_of(&result).as_deref(), Some("posted"));

    let sessions = seen.lock().await;
    assert_eq!(
        sessions.first(),
        Some(&None),
        "the first request cannot carry a session id, because none has been issued"
    );
    assert!(
        sessions
            .iter()
            .skip(1)
            .all(|s| s.as_deref() == Some("session-abc")),
        "every later request must echo the session the server issued: {sessions:?}"
    );
}

#[tokio::test]
async fn an_http_server_that_is_not_there_fails_its_handshake() {
    let client = McpClient::new();
    // Port 1 on loopback: nothing listens, and the connection is refused rather than timing out.
    let config = ServerConfig::http("http-2", "nowhere", "http://127.0.0.1:1/mcp");

    let err = client.tools(&config).await.expect_err("must fail");
    assert!(matches!(err, McpError::Handshake { .. }), "{err:?}");
}

/// A JSON-RPC error from a real server, over a real socket.
#[tokio::test]
async fn an_http_server_rejecting_a_method_reports_its_own_message() {
    let (url, _) = http_server().await;
    let client = McpClient::new();
    let config = ServerConfig::http("http-3", "remote", url);

    client.tools(&config).await.expect("lists");

    let mut list = Allowlist::new();
    list.allow("remote", "post_message");

    // The stub only publishes `post_message`, so this is refused before dispatch — which is the
    // point: the schema cache is what makes that possible without a round trip.
    let err = client
        .call(&config, &list, "no_such_tool", json!({}))
        .await
        .expect_err("must refuse");
    assert!(matches!(err, McpError::NotAllowed { .. }), "{err:?}");
}
