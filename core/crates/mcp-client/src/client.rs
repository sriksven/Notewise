//! Sessions: starting servers when they are first needed, and dispatching one call.
//!
//! # The gate is here, and there is one door
//!
//! [`McpClient::call`] is the only place in this crate that sends `tools/call`, and its first
//! statement is the allowlist check — before a process is spawned, before a socket is opened. A test
//! in this file fails if a second dispatch site ever appears, which is the same trick
//! `mcp-server` uses to stop `MUTATING_TOOLS` drifting from its dispatch table.
//!
//! # Lazy start
//!
//! Nothing runs until a tool is listed or called. Starting every configured server at launch would
//! make Notewise's startup depend on every MCP server the user has ever added, including the broken
//! one they added last week. Lazily, a misconfigured server breaks its own tools and nothing else.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock};

use crate::protocol::{self, initialize_params};
use crate::transport::{RealTransports, ServerConfig, Transport, TransportFactory};
use crate::{validate, Allowlist, McpError, Result, ToolDef};

/// The one dispatch method, named once.
///
/// Held as a constant so `every_dispatch_goes_through_the_gate` can prove there is a single site.
const TOOLS_CALL: &str = "tools/call";

/// How long any one call may take.
///
/// Generous, because an external tool may be filing a ticket over someone else's slow API, and
/// bounded, because a hung child process must not become a spinner with no end.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a server gets to finish its handshake.
///
/// Shorter than a call: a server that cannot introduce itself in ten seconds is broken, and the
/// user is waiting on a tool list to render.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// A started server.
#[derive(Debug)]
struct Session {
    /// The configured name, which is what the allowlist and the model use.
    name: String,
    /// What the server called itself during the handshake. Kept for display, because a server whose
    /// own name differs from the one the user typed is worth showing.
    reported_name: String,
    transport: Mutex<Box<dyn Transport>>,
    tools: RwLock<Vec<ToolDef>>,
}

impl Session {
    async fn open(
        factory: &dyn TransportFactory,
        config: &ServerConfig,
    ) -> Result<(Self, Vec<ToolDef>)> {
        let mut transport = factory.connect(config).await?;

        let initialized = transport
            .request("initialize", initialize_params())
            .await
            .map_err(|e| McpError::Handshake {
                server: config.name.clone(),
                detail: e.to_string(),
            })?;

        let reported_name = protocol::check_initialize(&config.name, &initialized)?;

        // Required by the protocol before any other request. A server is entitled to refuse
        // everything until it arrives.
        transport
            .notify("notifications/initialized", Value::Null)
            .await
            .map_err(|e| McpError::Handshake {
                server: config.name.clone(),
                detail: e.to_string(),
            })?;

        let listed = transport.request("tools/list", json!({})).await?;
        let tools = protocol::parse_tools(&listed);

        Ok((
            Self {
                name: config.name.clone(),
                reported_name,
                transport: Mutex::new(transport),
                tools: RwLock::new(tools.clone()),
            },
            tools,
        ))
    }

    async fn schema_for(&self, tool: &str) -> Option<Value> {
        self.tools
            .read()
            .await
            .iter()
            .find(|t| t.name == tool)
            .map(|t| t.input_schema.clone())
    }
}

/// What a caller can see about a running server without asking it anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningServer {
    pub id: String,
    pub name: String,
    pub reported_name: String,
    pub tool_count: usize,
}

/// Connections to external MCP servers.
///
/// Holds sessions — that is, child processes and sockets — and nothing else. Configuration and
/// history live in the database, and are handed in per call, so there is no in-memory copy of the
/// server list to drift out of step with the rows the user edited.
#[derive(Debug)]
pub struct McpClient {
    factory: Box<dyn TransportFactory>,
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
    call_timeout: Duration,
}

impl Default for McpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl McpClient {
    pub fn new() -> Self {
        Self::with_factory(Box::new(RealTransports::new()))
    }

    /// Use a different way of making transports. The seam tests connect a stub through.
    pub fn with_factory(factory: Box<dyn TransportFactory>) -> Self {
        Self {
            factory,
            sessions: Mutex::new(BTreeMap::new()),
            call_timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.call_timeout = timeout;
        self
    }

    /// Start a server whether or not it auto-starts, and return its tools.
    ///
    /// This is the path for a server pinned `auto_start: false`: the user asked for it explicitly.
    pub async fn start(&self, config: &ServerConfig) -> Result<Vec<ToolDef>> {
        let session = self.ensure(config, true).await?;
        let tools = session.tools.read().await.clone();
        Ok(tools)
    }

    /// The tools a server publishes, starting it if it is allowed to start.
    ///
    /// A server pinned off contributes nothing until somebody starts it. Its tools are absent from
    /// proposals rather than proposed and then refused, because a confirmation for a call that
    /// cannot run wastes the one thing this feature spends: the user's attention.
    pub async fn tools(&self, config: &ServerConfig) -> Result<Vec<ToolDef>> {
        let session = self.ensure(config, false).await?;
        let tools = session.tools.read().await.clone();
        Ok(tools)
    }

    /// Ask a running server for its tools again.
    ///
    /// Used after an upgrade changes what a server publishes. Not done automatically on every
    /// listing: the cached copy is what the confirmation dialog was rendered from, and re-fetching
    /// mid-flow could change a schema between propose and execute without anybody noticing.
    pub async fn refresh(&self, config: &ServerConfig) -> Result<Vec<ToolDef>> {
        let session = self.ensure(config, true).await?;
        let listed = {
            let mut transport = session.transport.lock().await;
            transport.request("tools/list", json!({})).await?
        };
        let tools = protocol::parse_tools(&listed);
        *session.tools.write().await = tools.clone();
        Ok(tools)
    }

    /// Call one tool.
    ///
    /// The order of the checks is the design:
    ///
    /// 1. the allowlist, before anything is started — a refused tool costs no process;
    /// 2. the tool exists on the server we are about to talk to;
    /// 3. the arguments still satisfy the schema *that server publishes now*, not the one they were
    ///    validated against when the proposal was made. A server upgraded in between is exactly
    ///    when a confirmed call should stop.
    pub async fn call(
        &self,
        config: &ServerConfig,
        allowlist: &Allowlist,
        tool: &str,
        arguments: Value,
    ) -> Result<Value> {
        allowlist.require(&config.name, tool)?;

        let session = self.ensure(config, false).await?;

        let schema = session
            .schema_for(tool)
            .await
            .ok_or_else(|| McpError::UnknownTool {
                server: config.name.clone(),
                tool: tool.to_string(),
            })?;

        // MCP wants an object. A tool taking no arguments gets `{}` rather than `null`, which some
        // servers reject as a malformed request.
        let arguments = if arguments.is_null() {
            json!({})
        } else {
            arguments
        };

        let problems = validate(&schema, &arguments);
        if !problems.is_empty() {
            return Err(McpError::InvalidArguments {
                tool: tool.to_string(),
                detail: problems
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
            });
        }

        let params = json!({ "name": tool, "arguments": arguments });

        let sent = {
            let mut transport = session.transport.lock().await;
            tokio::time::timeout(self.call_timeout, transport.request(TOOLS_CALL, params)).await
        };

        match sent {
            // A timeout is its own outcome, not a failure. The call may have taken effect, and
            // telling a user it failed is how a ticket gets filed twice.
            Err(_) => Err(McpError::Timeout {
                server: config.name.clone(),
                tool: tool.to_string(),
            }),
            Ok(result) => protocol::parse_tool_result(tool, result?),
        }
    }

    /// Stop a server, if it is running.
    pub async fn stop(&self, server_id: &str) -> bool {
        let session = self.sessions.lock().await.remove(server_id);
        match session {
            Some(session) => {
                // Best effort: whoever asked for the stop is not interested in why a dying process
                // would not say goodbye, and `kill_on_drop` handles the rest.
                if let Ok(mut transport) = session.transport.try_lock() {
                    transport.close().await;
                }
                true
            }
            None => false,
        }
    }

    /// Stop everything. Called when the app shuts down.
    pub async fn stop_all(&self) {
        let ids: Vec<String> = self.sessions.lock().await.keys().cloned().collect();
        for id in ids {
            self.stop(&id).await;
        }
    }

    pub async fn is_running(&self, server_id: &str) -> bool {
        self.sessions.lock().await.contains_key(server_id)
    }

    pub async fn running(&self) -> Vec<RunningServer> {
        let sessions = self.sessions.lock().await;
        let mut out = Vec::with_capacity(sessions.len());
        for (id, session) in sessions.iter() {
            out.push(RunningServer {
                id: id.clone(),
                name: session.name.clone(),
                reported_name: session.reported_name.clone(),
                tool_count: session.tools.read().await.len(),
            });
        }
        out
    }

    /// Get a session, starting one if permitted.
    ///
    /// The map lock is held across the connect. That serializes first-use of *different* servers
    /// behind each other, which is the price of not spawning the same server twice when two
    /// requests race — and spawning twice would leave an orphan process nobody has a handle to.
    /// The handshake is bounded by [`HANDSHAKE_TIMEOUT`] so the worst case is ten seconds and not
    /// forever.
    async fn ensure(&self, config: &ServerConfig, explicit: bool) -> Result<Arc<Session>> {
        config.validate()?;

        let mut sessions = self.sessions.lock().await;

        if let Some(session) = sessions.get(&config.id) {
            return Ok(session.clone());
        }

        if !config.auto_start && !explicit {
            return Err(McpError::NotStarted {
                server: config.name.clone(),
            });
        }

        let opened = tokio::time::timeout(HANDSHAKE_TIMEOUT, Session::open(&*self.factory, config))
            .await
            .map_err(|_| McpError::Handshake {
                server: config.name.clone(),
                detail: format!(
                    "the server did not finish its handshake within {}s",
                    HANDSHAKE_TIMEOUT.as_secs()
                ),
            })??;

        let (session, tools) = opened;
        tracing::info!(server = %config.name, tools = tools.len(), "MCP server started");

        let session = Arc::new(session);
        sessions.insert(config.id.clone(), session.clone());
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::TransportKind;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A server that answers from a script, so a test can choose what a server says.
    #[derive(Debug)]
    struct StubServer {
        tools: Value,
        call_result: Value,
        /// Set when the stub is asked to behave badly.
        protocol_version: String,
        calls: Arc<Mutex<Vec<(String, Value)>>>,
        /// Seconds to sleep before answering `tools/call`, for the timeout test.
        stall: Option<Duration>,
    }

    impl StubServer {
        fn new() -> Self {
            Self {
                tools: json!({
                    "tools": [{
                        "name": "create_issue",
                        "description": "File one",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "title": { "type": "string" } },
                            "required": ["title"]
                        }
                    }]
                }),
                call_result: json!({ "content": [{ "type": "text", "text": "ENG-1" }] }),
                protocol_version: protocol::PROTOCOL_VERSION.to_string(),
                calls: Arc::new(Mutex::new(Vec::new())),
                stall: None,
            }
        }
    }

    #[async_trait]
    impl Transport for StubServer {
        async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
            self.calls
                .lock()
                .await
                .push((method.to_string(), params.clone()));

            match method {
                "initialize" => Ok(json!({
                    "protocolVersion": self.protocol_version,
                    "serverInfo": { "name": "stub", "version": "0" }
                })),
                "tools/list" => Ok(self.tools.clone()),
                TOOLS_CALL => {
                    if let Some(stall) = self.stall {
                        tokio::time::sleep(stall).await;
                    }
                    Ok(self.call_result.clone())
                }
                other => Err(McpError::Rpc {
                    server: "stub".into(),
                    detail: format!("no method '{other}'"),
                }),
            }
        }

        async fn notify(&mut self, _method: &str, _params: Value) -> Result<()> {
            Ok(())
        }
    }

    /// Counts how many times a transport was actually made.
    #[derive(Debug)]
    struct CountingFactory {
        connects: Arc<AtomicUsize>,
        template: fn() -> StubServer,
    }

    impl CountingFactory {
        fn new() -> (Self, Arc<AtomicUsize>) {
            let connects = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    connects: connects.clone(),
                    template: StubServer::new,
                },
                connects,
            )
        }
    }

    #[async_trait]
    impl TransportFactory for CountingFactory {
        async fn connect(&self, _config: &ServerConfig) -> Result<Box<dyn Transport>> {
            self.connects.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new((self.template)()))
        }
    }

    /// A factory that always refuses, standing in for a missing binary.
    #[derive(Debug)]
    struct BrokenFactory;

    #[async_trait]
    impl TransportFactory for BrokenFactory {
        async fn connect(&self, config: &ServerConfig) -> Result<Box<dyn Transport>> {
            Err(McpError::SpawnFailed {
                server: config.name.clone(),
                detail: "no such file or directory".into(),
            })
        }
    }

    fn config() -> ServerConfig {
        ServerConfig::stdio("srv-1", "linear", "linear-mcp", vec![])
    }

    fn allowing(tool: &str) -> Allowlist {
        let mut list = Allowlist::new();
        list.allow("linear", tool);
        list
    }

    #[tokio::test]
    async fn a_handshake_lists_the_servers_tools() {
        let (factory, _) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));

        let tools = client.tools(&config()).await.expect("lists");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "create_issue");
    }

    /// Startup must not depend on every server the user ever added.
    #[tokio::test]
    async fn nothing_starts_until_something_needs_it() {
        let (factory, connects) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));

        assert_eq!(connects.load(Ordering::SeqCst), 0);
        assert!(client.running().await.is_empty());

        client.tools(&config()).await.expect("lists");
        assert_eq!(connects.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_second_use_reuses_the_running_server() {
        let (factory, connects) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));

        client.tools(&config()).await.expect("lists");
        client
            .call(
                &config(),
                &allowing("create_issue"),
                "create_issue",
                json!({"title":"x"}),
            )
            .await
            .expect("calls");

        assert_eq!(
            connects.load(Ordering::SeqCst),
            1,
            "one process, not one per request"
        );
    }

    /// A server pinned off contributes nothing until somebody starts it by hand.
    #[tokio::test]
    async fn a_server_pinned_off_does_not_start_on_first_use() {
        let (factory, connects) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));
        let pinned = config().with_auto_start(false);

        let err = client.tools(&pinned).await.expect_err("must not start");
        assert!(matches!(err, McpError::NotStarted { .. }), "{err:?}");
        assert_eq!(connects.load(Ordering::SeqCst), 0);

        let tools = client.start(&pinned).await.expect("explicit start works");
        assert_eq!(tools.len(), 1);
        assert_eq!(connects.load(Ordering::SeqCst), 1);
    }

    /// The property everything else rests on, and it must hold before a process exists.
    #[tokio::test]
    async fn a_tool_that_is_not_allowed_is_refused_before_anything_is_started() {
        let (factory, connects) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));

        let err = client
            .call(
                &config(),
                &Allowlist::new(),
                "create_issue",
                json!({"title":"x"}),
            )
            .await
            .expect_err("must refuse");

        assert!(matches!(err, McpError::NotAllowed { .. }), "{err:?}");
        assert_eq!(
            connects.load(Ordering::SeqCst),
            0,
            "a refused call must not cost a process"
        );
    }

    /// Enabling one tool must not enable its neighbours, at the dispatch site and not only in the
    /// allowlist's own unit tests.
    #[tokio::test]
    async fn enabling_one_tool_does_not_enable_another() {
        let (factory, _) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));

        let err = client
            .call(
                &config(),
                &allowing("create_issue"),
                "delete_issue",
                json!({}),
            )
            .await
            .expect_err("must refuse");
        assert!(matches!(err, McpError::NotAllowed { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn an_allowed_call_reaches_the_server_with_its_arguments() {
        let (factory, _) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));

        let result = client
            .call(
                &config(),
                &allowing("create_issue"),
                "create_issue",
                json!({ "title": "Fix the importer" }),
            )
            .await
            .expect("succeeds");

        assert_eq!(
            protocol::text_of(&result).as_deref(),
            Some("ENG-1"),
            "the server's own answer comes back"
        );
    }

    /// A server upgraded between propose and execute is exactly when a confirmed call should stop.
    #[tokio::test]
    async fn arguments_are_checked_against_the_schema_the_server_publishes_now() {
        let (factory, _) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));

        let err = client
            .call(
                &config(),
                &allowing("create_issue"),
                "create_issue",
                json!({ "titel": "typo" }),
            )
            .await
            .expect_err("must refuse");

        match err {
            McpError::InvalidArguments { tool, detail } => {
                assert_eq!(tool, "create_issue");
                assert!(detail.contains("title"), "{detail}");
            }
            other => panic!("expected invalid arguments, got {other:?}"),
        }
    }

    /// An enabled tool the server no longer publishes is named, not sent blindly.
    #[tokio::test]
    async fn a_tool_the_server_does_not_publish_is_refused() {
        let (factory, _) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));

        let mut list = Allowlist::new();
        list.allow("linear", "tool_that_vanished");

        let err = client
            .call(&config(), &list, "tool_that_vanished", json!({}))
            .await
            .expect_err("must refuse");
        assert!(matches!(err, McpError::UnknownTool { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn a_server_that_will_not_start_fails_its_own_tools_and_nothing_else() {
        let client = McpClient::with_factory(Box::new(BrokenFactory));

        let err = client.tools(&config()).await.expect_err("must fail");
        // Reported as a handshake failure with the spawn error inside: the caller wants one thing
        // to show the user, and "could not start" is that thing.
        let rendered = err.to_string();
        assert!(rendered.contains("no such file"), "{rendered}");
        assert!(!client.is_running("srv-1").await);
    }

    /// A timeout is its own outcome. Telling a user a call failed when it may have succeeded is
    /// how a ticket gets filed twice.
    #[tokio::test]
    async fn a_call_that_exceeds_the_timeout_is_a_timeout_and_not_a_failure() {
        #[derive(Debug)]
        struct StallingFactory;

        #[async_trait]
        impl TransportFactory for StallingFactory {
            async fn connect(&self, _config: &ServerConfig) -> Result<Box<dyn Transport>> {
                let mut stub = StubServer::new();
                stub.stall = Some(Duration::from_secs(30));
                Ok(Box::new(stub))
            }
        }

        let client = McpClient::with_factory(Box::new(StallingFactory))
            .with_timeout(Duration::from_millis(50));

        let err = client
            .call(
                &config(),
                &allowing("create_issue"),
                "create_issue",
                json!({"title":"x"}),
            )
            .await
            .expect_err("must time out");

        match err {
            McpError::Timeout { ref tool, .. } => assert_eq!(tool, "create_issue"),
            other => panic!("expected a timeout, got {other:?}"),
        }
        assert!(
            err.outcome_unknown(),
            "a timeout must not claim nothing happened"
        );
    }

    /// A tool reporting its own failure inside a successful response, end to end.
    #[tokio::test]
    async fn a_tool_error_is_not_recorded_as_a_success() {
        #[derive(Debug)]
        struct FailingFactory;

        #[async_trait]
        impl TransportFactory for FailingFactory {
            async fn connect(&self, _config: &ServerConfig) -> Result<Box<dyn Transport>> {
                let mut stub = StubServer::new();
                stub.call_result = json!({
                    "isError": true,
                    "content": [{ "type": "text", "text": "project not found" }]
                });
                Ok(Box::new(stub))
            }
        }

        let client = McpClient::with_factory(Box::new(FailingFactory));
        let err = client
            .call(
                &config(),
                &allowing("create_issue"),
                "create_issue",
                json!({"title":"x"}),
            )
            .await
            .expect_err("must not succeed");

        assert!(matches!(err, McpError::ToolError { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn a_version_mismatch_stops_the_session() {
        #[derive(Debug)]
        struct OldFactory;

        #[async_trait]
        impl TransportFactory for OldFactory {
            async fn connect(&self, _config: &ServerConfig) -> Result<Box<dyn Transport>> {
                let mut stub = StubServer::new();
                stub.protocol_version = "1999-01-01".into();
                Ok(Box::new(stub))
            }
        }

        let client = McpClient::with_factory(Box::new(OldFactory));
        let err = client.tools(&config()).await.expect_err("must refuse");
        assert!(matches!(err, McpError::Handshake { .. }), "{err:?}");
        assert!(
            !client.is_running("srv-1").await,
            "a failed handshake must not leave a session behind"
        );
    }

    #[tokio::test]
    async fn a_stopped_server_starts_again_on_next_use() {
        let (factory, connects) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));

        client.tools(&config()).await.expect("lists");
        assert!(client.stop("srv-1").await);
        assert!(!client.is_running("srv-1").await);
        assert!(
            !client.stop("srv-1").await,
            "stopping twice is not an error"
        );

        client.tools(&config()).await.expect("lists again");
        assert_eq!(connects.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn stopping_everything_leaves_nothing_running() {
        let (factory, _) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));

        client.tools(&config()).await.expect("lists");
        client
            .tools(&ServerConfig::stdio("srv-2", "other", "other-mcp", vec![]))
            .await
            .expect("lists");
        assert_eq!(client.running().await.len(), 2);

        client.stop_all().await;
        assert!(client.running().await.is_empty());
    }

    #[tokio::test]
    async fn a_running_server_reports_the_name_it_gave_itself() {
        let (factory, _) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));
        client.tools(&config()).await.expect("lists");

        let running = client.running().await;
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].name, "linear");
        assert_eq!(running[0].reported_name, "stub");
        assert_eq!(running[0].tool_count, 1);
    }

    #[tokio::test]
    async fn a_misconfigured_server_is_refused_without_a_connect_attempt() {
        let (factory, connects) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));

        let broken = ServerConfig {
            id: "srv-9".into(),
            name: "broken".into(),
            transport: TransportKind::Stdio {
                command: String::new(),
                args: vec![],
                env: BTreeMap::new(),
            },
            auto_start: true,
        };

        assert!(client.tools(&broken).await.is_err());
        assert_eq!(connects.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn refreshing_picks_up_a_tool_list_that_changed() {
        let (factory, _) = CountingFactory::new();
        let client = McpClient::with_factory(Box::new(factory));

        assert_eq!(client.tools(&config()).await.expect("lists").len(), 1);
        // The stub answers the same way, so this proves the refresh path runs and replaces the
        // cache rather than that the contents changed.
        assert_eq!(client.refresh(&config()).await.expect("refreshes").len(), 1);
    }

    /// The drift test.
    ///
    /// `mcp-server` keeps `MUTATING_TOOLS` as data so its write check cannot fall out of step with
    /// its dispatch table. The same hazard here is a second place that sends `tools/call` without
    /// consulting the allowlist first. So the method name appears as a string exactly once in this
    /// crate — in `TOOLS_CALL` — and any new dispatch site has to use the constant, whose only
    /// caller is [`McpClient::call`], which begins with the gate.
    ///
    /// The needle is assembled rather than written out, so this test does not match itself.
    #[test]
    fn every_dispatch_goes_through_the_gate() {
        let needle = concat!("\"tools", "/call\"");
        let sources = [
            include_str!("client.rs"),
            include_str!("transport.rs"),
            include_str!("protocol.rs"),
            include_str!("lib.rs"),
        ];

        let occurrences: usize = sources
            .iter()
            .map(|source| source.matches(needle).count())
            .sum();

        assert_eq!(
            occurrences, 1,
            "'tools/call' must appear once, as TOOLS_CALL. A second literal means a dispatch \
             site that may not have consulted the allowlist."
        );
    }
}
