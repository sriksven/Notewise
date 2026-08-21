//! Getting bytes to an external MCP server and a reply back.
//!
//! # Two transports, one interface
//!
//! Stdio is the common case: a local tool the app launches as a child process. Streamable HTTP is
//! what remote servers speak. Both reduce to "send a request, get the reply with the matching id",
//! which is what [`Transport`] is.
//!
//! # Where the real bugs live
//!
//! Not in the framing. In the child process: a server that writes a megabyte to stderr and blocks
//! because nobody drained it, a server that ignores SIGTERM, a process left running after the app
//! quits. Each of those is handled here on purpose rather than discovered later —
//! [`StdioTransport`] drains stderr in its own task and sets `kill_on_drop`.
//!
//! # Why a factory
//!
//! Tests need a server whose replies they choose, and there is no honest way to get one out of a
//! real transport. [`TransportFactory`] is the seam: [`RealTransports`] spawns processes and opens
//! sockets, and a test supplies something that does neither.

use std::collections::BTreeMap;
use std::process::Stdio;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::protocol::{Incoming, Outgoing};
use crate::{McpError, Result};

/// Where a server is and how to reach it.
///
/// Credentials — environment variables for a stdio server, headers for an HTTP one — arrive here
/// already resolved. They are held in the keychain and the caller fetches them; nothing in this
/// crate reads or writes a credential store, and nothing persists these values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// Stable identifier, used as the session key. The database row's id in practice.
    pub id: String,
    /// What a human and a model call this server. The allowlist is keyed by this.
    pub name: String,
    pub transport: TransportKind,
    /// Whether first use may start it.
    ///
    /// `false` for a resource-heavy server the user wants to start by hand. Such a server's tools
    /// are absent from proposals until it is running, which is the honest outcome: proposing a call
    /// to something that will not start wastes a confirmation.
    pub auto_start: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportKind {
    Stdio {
        command: String,
        args: Vec<String>,
        /// Extra environment for the child, on top of the inherited environment.
        ///
        /// Inherited rather than cleared: a server launched without `PATH` or `HOME` fails in ways
        /// that look like a protocol bug.
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

impl ServerConfig {
    /// A stdio server, auto-starting.
    pub fn stdio(
        id: impl Into<String>,
        name: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            transport: TransportKind::Stdio {
                command: command.into(),
                args,
                env: BTreeMap::new(),
            },
            auto_start: true,
        }
    }

    /// An HTTP server, auto-starting.
    pub fn http(id: impl Into<String>, name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            transport: TransportKind::Http {
                url: url.into(),
                headers: BTreeMap::new(),
            },
            auto_start: true,
        }
    }

    pub fn with_auto_start(mut self, auto_start: bool) -> Self {
        self.auto_start = auto_start;
        self
    }

    /// Whether this configuration could work at all.
    ///
    /// Checked before spawning so an empty command produces a message about the command rather
    /// than an operating-system error nobody can act on.
    pub fn validate(&self) -> Result<()> {
        match &self.transport {
            TransportKind::Stdio { command, .. } if command.trim().is_empty() => {
                Err(McpError::Misconfigured {
                    server: self.name.clone(),
                    detail: "a stdio server needs a command to run".into(),
                })
            }
            TransportKind::Http { url, .. }
                if !(url.starts_with("http://") || url.starts_with("https://")) =>
            {
                Err(McpError::Misconfigured {
                    server: self.name.clone(),
                    detail: format!("'{url}' is not an http or https URL"),
                })
            }
            _ => Ok(()),
        }
    }
}

/// One request, one reply.
///
/// `&mut self` rather than `&self`: a stdio pipe carries one conversation, and two concurrent calls
/// interleaving their writes would corrupt both. The caller holds the lock, so the serialization is
/// visible where it is decided rather than hidden in each implementation.
#[async_trait]
pub trait Transport: Send + std::fmt::Debug {
    async fn request(&mut self, method: &str, params: Value) -> Result<Value>;

    /// Fire and forget. Used for `notifications/initialized`, which the protocol requires and
    /// which takes no reply.
    async fn notify(&mut self, method: &str, params: Value) -> Result<()>;

    /// Best-effort teardown. A transport that cannot close cleanly must not fail a caller who is
    /// already finished with it.
    async fn close(&mut self) {}
}

/// How a [`Transport`] gets made.
#[async_trait]
pub trait TransportFactory: Send + Sync + std::fmt::Debug {
    async fn connect(&self, config: &ServerConfig) -> Result<Box<dyn Transport>>;
}

/// Child processes and real sockets.
#[derive(Debug, Default)]
pub struct RealTransports {
    http: reqwest::Client,
}

impl RealTransports {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl TransportFactory for RealTransports {
    async fn connect(&self, config: &ServerConfig) -> Result<Box<dyn Transport>> {
        config.validate()?;

        match &config.transport {
            TransportKind::Stdio { command, args, env } => Ok(Box::new(StdioTransport::spawn(
                &config.name,
                command,
                args,
                env,
            )?)),
            TransportKind::Http { url, headers } => Ok(Box::new(HttpTransport::new(
                self.http.clone(),
                &config.name,
                url,
                headers,
            ))),
        }
    }
}

// ---------------------------------------------------------------- stdio

/// A server running as a child process, spoken to over newline-delimited JSON.
#[derive(Debug)]
pub struct StdioTransport {
    server: String,
    /// Held so the process is killed when this is dropped.
    _child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl StdioTransport {
    fn spawn(
        server: &str,
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> Result<Self> {
        let mut child = Command::new(command)
            .args(args)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Without this, quitting Notewise leaves every server it ever started running. The
            // user has no window to close them from and no reason to suspect they exist.
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| McpError::SpawnFailed {
                server: server.to_string(),
                detail: source.to_string(),
            })?;

        let stdin = child.stdin.take().ok_or_else(|| McpError::SpawnFailed {
            server: server.to_string(),
            detail: "the child has no stdin".into(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| McpError::SpawnFailed {
            server: server.to_string(),
            detail: "the child has no stdout".into(),
        })?;

        // Drain stderr into the log. This is not for diagnostics — it is what stops a server that
        // logs verbosely from filling the pipe buffer and blocking forever on its next write,
        // which looks exactly like a hung tool call.
        if let Some(stderr) = child.stderr.take() {
            let name = server.to_string();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(server = %name, "{line}");
                }
            });
        }

        Ok(Self {
            server: server.to_string(),
            _child: child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            next_id: 1,
        })
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    async fn write(&mut self, message: &Outgoing) -> Result<()> {
        let mut line = serde_json::to_string(message)?;
        line.push('\n');

        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| self.broken(e))?;
        self.stdin.flush().await.map_err(|e| self.broken(e))
    }

    fn broken(&self, source: std::io::Error) -> McpError {
        McpError::Transport {
            server: self.server.clone(),
            detail: source.to_string(),
        }
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.take_id();
        let message = Outgoing::request(id, method, params);
        self.write(&message).await?;

        // Read until the matching reply. Log notifications and other ids are skipped rather than
        // returned — a read loop that took the first line would hand a log entry back as a result.
        loop {
            let line = self
                .stdout
                .next_line()
                .await
                .map_err(|e| self.broken(e))?
                .ok_or_else(|| McpError::Transport {
                    server: self.server.clone(),
                    detail: format!("the server closed its output while '{method}' was in flight"),
                })?;

            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<Incoming>(&line) {
                Ok(incoming) if incoming.is_reply_to(id) => {
                    return incoming.into_result(&self.server)
                }
                Ok(_) => continue,
                Err(e) => {
                    // Not fatal: a server that prints a banner to stdout before speaking protocol
                    // is common, and killing the session over it would make such a server
                    // permanently unusable.
                    tracing::debug!(server = %self.server, error = %e, "ignoring a non-JSON line");
                }
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let message = Outgoing::notification(method, params);
        self.write(&message).await
    }

    async fn close(&mut self) {
        // Closing stdin is how a well-behaved MCP server learns to exit. `kill_on_drop` is the
        // backstop for one that does not.
        let _ = self.stdin.shutdown().await;
    }
}

// ---------------------------------------------------------------- http

/// A remote server over streamable HTTP.
#[derive(Debug)]
pub struct HttpTransport {
    client: reqwest::Client,
    server: String,
    url: String,
    headers: BTreeMap<String, String>,
    /// Assigned by the server during `initialize` and echoed on every later request.
    session_id: Option<String>,
    next_id: u64,
}

impl HttpTransport {
    fn new(
        client: reqwest::Client,
        server: &str,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Self {
        Self {
            client,
            server: server.to_string(),
            url: url.to_string(),
            headers: headers.clone(),
            session_id: None,
            next_id: 1,
        }
    }

    async fn post(&mut self, message: &Outgoing) -> Result<Option<String>> {
        let mut request = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            // Both, because a streamable-HTTP server chooses which to answer with per request.
            .header("accept", "application/json, text/event-stream");

        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Some(session) = &self.session_id {
            request = request.header("mcp-session-id", session);
        }

        let response = request
            .json(message)
            .send()
            .await
            .map_err(|e| McpError::Transport {
                server: self.server.clone(),
                detail: e.to_string(),
            })?;

        // Captured before the body is consumed.
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session_id = Some(session.to_string());
        }

        let status = response.status();
        let sse = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/event-stream"));

        let body = response.text().await.map_err(|e| McpError::Transport {
            server: self.server.clone(),
            detail: e.to_string(),
        })?;

        if !status.is_success() {
            return Err(McpError::Transport {
                server: self.server.clone(),
                detail: format!("HTTP {status}: {}", body.trim()),
            });
        }

        // 202 with an empty body is the correct answer to a notification.
        if body.trim().is_empty() {
            return Ok(None);
        }

        let payload = if sse {
            first_sse_data(&body)
        } else {
            Some(body)
        };

        Ok(payload)
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// Pull the first `data:` payload out of an SSE body.
///
/// Only the first: this client makes one request at a time and reads one reply, so a stream
/// carrying several events is carrying notifications alongside the answer, and the answer is
/// matched by id afterwards.
fn first_sse_data(body: &str) -> Option<String> {
    let mut collected: Vec<String> = Vec::new();

    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            collected.push(rest.trim_start().to_string());
        } else if line.trim().is_empty() && !collected.is_empty() {
            // A blank line ends the event.
            break;
        }
    }

    if collected.is_empty() {
        None
    } else {
        Some(collected.join("\n"))
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.take_id();
        let message = Outgoing::request(id, method, params);

        let body = self
            .post(&message)
            .await?
            .ok_or_else(|| McpError::Transport {
                server: self.server.clone(),
                detail: format!("the server answered '{method}' with an empty body"),
            })?;

        let incoming: Incoming =
            serde_json::from_str(body.trim()).map_err(|e| McpError::Transport {
                server: self.server.clone(),
                detail: format!("could not read the reply to '{method}': {e}"),
            })?;

        if !incoming.is_reply_to(id) {
            return Err(McpError::Transport {
                server: self.server.clone(),
                detail: format!("the server answered '{method}' with someone else's reply"),
            });
        }

        incoming.into_result(&self.server)
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.post(&Outgoing::notification(method, params)).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stdio_server_without_a_command_is_refused_before_spawning() {
        let config = ServerConfig::stdio("1", "broken", "  ", vec![]);
        let err = config.validate().expect_err("must refuse");
        assert!(err.to_string().contains("command"), "{err}");
    }

    #[test]
    fn a_non_http_url_is_refused() {
        let config = ServerConfig::http("1", "broken", "ftp://example.com/mcp");
        let err = config.validate().expect_err("must refuse");
        assert!(err.to_string().contains("ftp://example.com/mcp"), "{err}");
    }

    #[test]
    fn a_usable_configuration_validates() {
        assert!(ServerConfig::stdio("1", "linear", "linear-mcp", vec![])
            .validate()
            .is_ok());
        assert!(ServerConfig::http("2", "remote", "https://example.com/mcp")
            .validate()
            .is_ok());
    }

    #[test]
    fn auto_start_can_be_turned_off() {
        let config = ServerConfig::stdio("1", "heavy", "heavy-mcp", vec![]).with_auto_start(false);
        assert!(!config.auto_start);
    }

    #[test]
    fn an_sse_event_yields_its_data_line() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
        assert_eq!(
            first_sse_data(body).as_deref(),
            Some(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#)
        );
    }

    #[test]
    fn a_multi_line_data_field_is_rejoined() {
        let body = "data: {\"a\":\ndata: 1}\n\n";
        assert_eq!(first_sse_data(body).as_deref(), Some("{\"a\":\n1}"));
    }

    #[test]
    fn a_body_with_no_data_lines_has_no_payload() {
        assert_eq!(first_sse_data(": keep-alive\n\n"), None);
    }

    /// Only the first event: a later one is a notification, not the answer.
    #[test]
    fn a_second_event_is_left_alone() {
        let body = "data: first\n\ndata: second\n\n";
        assert_eq!(first_sse_data(body).as_deref(), Some("first"));
    }
}
