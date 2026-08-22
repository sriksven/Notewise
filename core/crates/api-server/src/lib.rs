//! Local REST API over the Notewise engine.
//!
//! Binds to **loopback only**. Any process on the machine — the browser extension, a script,
//! the desktop app's frontend — can reach the running engine; nothing off the machine can.
//! See [`Server::bind`], which refuses a non-loopback address rather than quietly exposing a
//! user's meetings to their network.
//!
//! # Example
//!
//! ```
//! use notewise_api_server::{AppState, Server};
//! use notewise_ai_router::{Router, RouterConfig};
//! use notewise_storage::Database;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let state = AppState::new(
//!     Database::open_in_memory()?,
//!     Router::from_config(RouterConfig::mock())?,
//! );
//!
//! // Refuses to serve a user's meetings to the network.
//! assert!(Server::bind("0.0.0.0:8080").is_err());
//! assert!(Server::bind("127.0.0.1:0").is_ok());
//! # let _ = state;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod agent;
mod ask;
pub mod assistant;
pub mod calendar;
mod connectors;
pub mod diarization;
pub mod dictation;
pub mod downloads;
mod error;
pub mod indexing;
pub mod jobs;
pub mod join;
pub mod memory;
pub mod recording;
mod retrieval;
mod routes;
mod routing;
pub mod schedule;
mod setup;
pub mod speakers;
mod state;
pub mod sync;
mod tools;
pub mod vault;
mod voiceprints;
mod workspace;

pub use downloads::{DownloadManager, DownloadState, DownloadStatus};
pub use error::{ApiError, ApiResult};
pub use recording::{RecordingError, RecordingManager};
pub use speakers::{PendingTimeline, PendingTimelines, SpeakerEvents};
pub use state::{stored_routes, AppState, BACKEND_KIND_KEY, BACKEND_MODEL_KEY, ROUTING_RULES_KEY};

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router as AxumRouter;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServeError {
    #[error("'{0}' is not a valid socket address")]
    InvalidAddress(String),

    #[error(
        "refusing to bind {0}: the API server serves unauthenticated access to the user's \
         meetings and must stay on loopback"
    )]
    NotLoopback(SocketAddr),

    #[error("could not bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },

    #[error("server error: {0}")]
    Io(#[from] std::io::Error),
}

/// A validated, loopback-only bind address.
///
/// A newtype rather than a bare `SocketAddr` so the check cannot be bypassed by
/// constructing an address elsewhere and passing it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Server {
    addr: SocketAddr,
}

/// Start every background loop.
///
/// One function rather than the same four lines at each entry point. There are three ways to start
/// this server — `serve`, `serve_with_frontend`, `bind_with_frontend` — and while the list was
/// duplicated across all three, adding a loop meant remembering all three. A loop wired into two of
/// them is the worst outcome available: it works when you test it and does nothing in the build
/// users run.
///
/// The loops start with the *server*, not with the router. `app` is also called by tests and by an
/// embedder that only wants the route table, and neither wants background work running.
///
/// `every_spawn_is_started` below reads this function's own source and fails if a module grows a
/// `spawn` that is never called from here.
fn start_background(state: &Arc<AppState>) {
    crate::jobs::spawn(Arc::clone(state));
    crate::join::spawn(Arc::clone(state));
    crate::memory::spawn(Arc::clone(state));
    crate::sync::spawn(Arc::clone(state));
}

impl Server {
    /// Default port. Registered nowhere — chosen to sit clear of common dev servers.
    pub const DEFAULT_PORT: u16 = 47_821;

    /// Validate a bind address.
    ///
    /// Returns [`ServeError::NotLoopback`] for anything that would accept connections from
    /// off the machine. The API is unauthenticated by design — it assumes a trust boundary at
    /// the machine edge — so binding it to `0.0.0.0` would publish the user's meetings to
    /// their whole network.
    pub fn bind(addr: impl AsRef<str>) -> Result<Self, ServeError> {
        let addr = addr.as_ref();
        let parsed: SocketAddr = addr
            .parse()
            .map_err(|_| ServeError::InvalidAddress(addr.to_string()))?;

        if !parsed.ip().is_loopback() {
            return Err(ServeError::NotLoopback(parsed));
        }

        Ok(Self { addr: parsed })
    }

    /// How many ports above the default are considered discoverable.
    ///
    /// The browser extension has to find the engine without being told where it is, and a
    /// Manifest V3 `host_permissions` list is static — it cannot name a port chosen at runtime.
    /// A small, fixed window is the only shape that satisfies both: the engine lands somewhere
    /// inside it, and the extension can enumerate it.
    ///
    /// Ten, not one, because a `notewise serve` may already hold the default and the desktop app
    /// failing to launch over that would be a poor trade. Ten, not a hundred, because every entry
    /// is a permission the extension has to be granted and a request it may have to make.
    pub const DISCOVERY_PORTS: u16 = 10;

    /// Bind to `127.0.0.1` on the default port.
    pub fn local() -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], Self::DEFAULT_PORT)),
        }
    }

    /// Bind the first free port in the discoverable window, falling back to any free port.
    ///
    /// The desktop shell used to ask for port 0 — a free port chosen by the OS — which avoided
    /// collisions and made the engine unfindable: the extension had one hardcoded port and the
    /// app was never on it.
    ///
    /// The fallback keeps the original property. If all ten are taken the app still starts; it
    /// is simply not discoverable, which is better than refusing to open.
    pub fn discoverable() -> Self {
        for offset in 0..Self::DISCOVERY_PORTS {
            let port = Self::DEFAULT_PORT + offset;
            // Bound and dropped immediately: this is a probe, and the real listener is created
            // later by `bind_with_frontend`. A port that frees in between is a race we lose by
            // failing to start, which the caller reports — not by binding something unexpected.
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                return Self {
                    addr: SocketAddr::from(([127, 0, 0, 1], port)),
                };
            }
        }

        tracing::warn!(
            "ports {}-{} are all in use; the browser extension will not find this engine",
            Self::DEFAULT_PORT,
            Self::DEFAULT_PORT + Self::DISCOVERY_PORTS - 1
        );
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Serve until the process is signalled.
    pub async fn serve(self, state: AppState) -> Result<(), ServeError> {
        let state = Arc::new(state);
        start_background(&state);
        self.serve_router(app(Arc::clone(&state)), state).await
    }

    /// Serve the API plus a frontend from `dir`.
    pub async fn serve_with_frontend(
        self,
        state: AppState,
        dir: impl AsRef<std::path::Path>,
    ) -> Result<(), ServeError> {
        let state = Arc::new(state);
        start_background(&state);
        self.serve_router(app_with_frontend(Arc::clone(&state), dir), state)
            .await
    }

    async fn serve_router(
        self,
        router: AxumRouter,
        state: Arc<AppState>,
    ) -> Result<(), ServeError> {
        let listener = tokio::net::TcpListener::bind(self.addr)
            .await
            .map_err(|source| ServeError::Bind {
                addr: self.addr,
                source,
            })?;

        let bound = listener.local_addr()?;
        tracing::info!(addr = %bound, "notewise api listening on loopback");

        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal())
            .await?;

        // Every MCP server this session started is a child process. `kill_on_drop` is the backstop,
        // but a process left for the allocator to notice is a process the user cannot see and has
        // no window to close — so they are stopped here, on the way out, on purpose.
        state.mcp().stop_all().await;

        Ok(())
    }

    /// Bind now and return the router-serving future plus the real address.
    ///
    /// Binding before returning is what lets an embedder (the desktop shell) know the port is
    /// actually listening before it points a window at it — otherwise the window can load
    /// before the server is up and show a connection error on launch.
    pub async fn bind_with_frontend(
        self,
        state: AppState,
        dir: impl AsRef<std::path::Path>,
    ) -> Result<
        (
            SocketAddr,
            impl std::future::Future<Output = Result<(), ServeError>>,
        ),
        ServeError,
    > {
        let listener = tokio::net::TcpListener::bind(self.addr)
            .await
            .map_err(|source| ServeError::Bind {
                addr: self.addr,
                source,
            })?;

        let bound = listener.local_addr()?;
        let state = Arc::new(state);
        start_background(&state);
        let router = app_with_frontend(Arc::clone(&state), dir);

        Ok((bound, async move {
            axum::serve(listener, router).await?;
            state.mcp().stop_all().await;
            Ok(())
        }))
    }
}

/// Build the route table.
///
/// Public so tests and embedders (the desktop app runs this in-process) can drive the API
/// without opening a socket.
pub fn app(state: Arc<AppState>) -> AxumRouter {
    routes::router(state)
}

/// Build the route table and serve a frontend from `dir` alongside it.
///
/// Serving the UI from the engine is a security decision, not a convenience. The alternative
/// — the frontend on its own origin calling this API — is cross-origin, which would require
/// permissive CORS on an unauthenticated loopback server. That would let **any** page the user
/// visits read their meetings, since same-origin policy is currently the only thing standing
/// in the way. Same origin means no CORS is needed at all.
///
/// Unknown paths fall back to `index.html` so client-side routing works on a hard refresh.
pub fn app_with_frontend(state: Arc<AppState>, dir: impl AsRef<std::path::Path>) -> AxumRouter {
    let dir = dir.as_ref();
    let index = dir.join("index.html");

    // The fallback is a handler rather than `ServeDir::not_found_service(ServeFile::new(..))`,
    // which serves the right body but keeps the 404 status. A deep link answered with 404 is
    // wrong twice over: the page did load, and crawlers, `curl --fail`, and the webview's own
    // error handling all key off the status rather than the body.
    let spa = axum::routing::any(move || {
        let index = index.clone();
        async move { serve_index(index).await }
    });

    routes::router(state).fallback_service(tower_http::services::ServeDir::new(dir).fallback(spa))
}

/// Serve `index.html` with a 200, or say plainly that the frontend was never built.
async fn serve_index(index: std::path::PathBuf) -> axum::response::Response {
    use axum::response::IntoResponse;

    match tokio::fs::read(&index).await {
        Ok(bytes) => (
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            bytes,
        )
            .into_response(),
        Err(e) => {
            tracing::error!(path = %index.display(), error = %e, "frontend index is missing");
            (
                axum::http::StatusCode::NOT_FOUND,
                "the Notewise frontend is not built — run `npm run build` in apps/desktop",
            )
                .into_response()
        }
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    /// Every module that defines a background loop has it started.
    ///
    /// Reads the crate's own source rather than asserting a hand-written list, because a
    /// hand-written list is the thing that goes stale. Twice in this crate's history a loop existed
    /// and ran for nobody: audio retention swept nothing because `sweep_audio` had no caller, and
    /// the memory reflector never reflected. Both compiled, both passed clippy, and both were `pub`
    /// so dead-code analysis said nothing.
    ///
    /// The rule this encodes: if a module says `pub fn spawn`, `start_background` calls it.
    #[test]
    fn every_spawn_is_started() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

        let mut defines_a_loop = Vec::new();
        for entry in std::fs::read_dir(&src).expect("the crate has a src directory") {
            let path = entry.expect("a readable directory entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            // Skipped: this file holds `start_background`, not a loop — and it holds this test,
            // whose own source would otherwise match the needle below. A drift test that matches
            // itself reports drift that does not exist.
            if path.file_name().and_then(|n| n.to_str()) == Some("lib.rs") {
                continue;
            }

            let text = std::fs::read_to_string(&path).expect("a readable source file");
            // Split so that no file containing this test can match on the test's own text.
            if text.contains(concat!("pub fn ", "spawn(")) {
                let module = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .expect("a utf-8 file name")
                    .to_string();
                defines_a_loop.push(module);
            }
        }

        assert!(
            !defines_a_loop.is_empty(),
            "found no background loops at all, so this test is no longer checking anything"
        );

        // Just this function's body, not the whole file: the call has to be in the one place every
        // entry point goes through. A stray `jobs::spawn` somewhere else is what this rules out.
        let body = {
            let start = src.join("lib.rs");
            let text = std::fs::read_to_string(start).expect("a readable lib.rs");
            let head = text
                .find("fn start_background(")
                .expect("start_background still exists");
            let rest = &text[head..];
            let end = rest
                .find("\n}\n")
                .expect("the function is brace-terminated");
            rest[..end].to_string()
        };

        for module in defines_a_loop {
            assert!(
                body.contains(&format!("{module}::spawn(")),
                "`{module}` defines `pub fn spawn` but `start_background` never calls it, so the \
                 loop runs for nobody. Add it there, or make the spawn private."
            );
        }
    }

    /// Every way of starting the server starts the background loops.
    ///
    /// The counterpart to the test above: that one catches a loop nobody starts, this one catches an
    /// entry point that starts nothing. Both failures look identical from the outside — a feature
    /// that quietly never happens — and this is the half that a fourth `serve`-shaped method would
    /// reintroduce.
    #[test]
    fn every_entry_point_starts_them() {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
        )
        .expect("a readable lib.rs");

        // The entry points are the `Server` methods that bind a listener. `serve_router` is the
        // shared tail and is reached only from `serve`/`serve_with_frontend`, so it is not one.
        let entry_points = ["serve", "serve_with_frontend", "bind_with_frontend"];

        for name in entry_points {
            let at = text
                .find(&format!("pub async fn {name}("))
                .unwrap_or_else(|| panic!("`{name}` still exists"));
            // Up to the end of that method: the next line that starts a new item at indent 4.
            let rest = &text[at..];
            let end = rest[1..]
                .find("\n    /// ")
                .or_else(|| rest[1..].find("\n    pub "))
                .or_else(|| rest[1..].find("\n    async fn "))
                .map(|i| i + 1)
                .unwrap_or(rest.len());
            assert!(
                rest[..end].contains("start_background(&state)"),
                "`{name}` binds a listener but never starts the background loops, so a server \
                 started this way does no scheduled work at all"
            );
        }
    }

    use super::*;

    #[test]
    fn loopback_addresses_are_accepted() {
        for addr in ["127.0.0.1:8080", "127.0.0.1:0", "[::1]:8080"] {
            assert!(Server::bind(addr).is_ok(), "{addr} should be accepted");
        }
    }

    #[test]
    fn non_loopback_addresses_are_refused() {
        // Each of these would expose the user's meetings beyond their machine.
        for addr in ["0.0.0.0:8080", "192.168.1.10:8080", "[::]:8080"] {
            let err = Server::bind(addr).expect_err("{addr} should be refused");
            assert!(
                matches!(err, ServeError::NotLoopback(_)),
                "{addr} gave {err:?}"
            );
        }
    }

    #[test]
    fn malformed_addresses_are_rejected() {
        for addr in ["not-an-address", "127.0.0.1", ""] {
            assert!(matches!(
                Server::bind(addr).expect_err("should be rejected"),
                ServeError::InvalidAddress(_)
            ));
        }
    }

    /// The window has to be reachable, which means loopback and inside the range the browser
    /// extension is granted permission for. A port outside it is an engine nothing can find.
    #[test]
    fn a_discoverable_engine_lands_in_the_advertised_window() {
        let server = Server::discoverable();
        assert!(server.addr().ip().is_loopback());

        let port = server.addr().port();
        let last = Server::DEFAULT_PORT + Server::DISCOVERY_PORTS - 1;
        assert!(
            (Server::DEFAULT_PORT..=last).contains(&port) || port == 0,
            "got {port}, which is neither in {}..={last} nor the all-taken fallback",
            Server::DEFAULT_PORT
        );
    }

    /// A port already in use must be stepped over, not failed on. The whole reason the shell
    /// used port 0 was that a running `notewise serve` should not stop the app from opening.
    #[test]
    fn an_occupied_port_is_skipped() {
        let held = std::net::TcpListener::bind(("127.0.0.1", Server::DEFAULT_PORT));

        // Only meaningful if this test could actually take the default port; if something else
        // on the machine holds it, the assertion below still holds for a different reason.
        let server = Server::discoverable();
        if held.is_ok() {
            assert_ne!(
                server.addr().port(),
                Server::DEFAULT_PORT,
                "the default was held, so discovery must have moved on"
            );
        }
        assert!(server.addr().ip().is_loopback());
    }

    #[test]
    fn the_default_is_loopback() {
        let server = Server::local();
        assert!(server.addr().ip().is_loopback());
        assert_eq!(server.addr().port(), Server::DEFAULT_PORT);
    }

    #[test]
    fn refusal_message_explains_why() {
        let err = Server::bind("0.0.0.0:8080").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("loopback"), "{message}");
        assert!(message.contains("unauthenticated"), "{message}");
    }

    mod frontend {
        use super::*;
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use notewise_ai_router::{Router, RouterConfig};
        use notewise_storage::Database;
        use tower::ServiceExt;

        /// A throwaway `dist` directory, so these tests do not depend on the frontend
        /// having been built.
        fn dist() -> tempfile::TempDir {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(
                dir.path().join("index.html"),
                "<!doctype html><title>NW</title>",
            )
            .expect("index");
            std::fs::create_dir_all(dir.path().join("assets")).expect("assets");
            std::fs::write(dir.path().join("assets/app.js"), "export const x = 1;").expect("asset");
            dir
        }

        fn router(dir: &tempfile::TempDir) -> AxumRouter {
            let state = AppState::new(
                Database::open_in_memory().expect("db"),
                Router::from_config(RouterConfig::mock()).expect("router"),
            );
            app_with_frontend(Arc::new(state), dir.path())
        }

        async fn get(dir: &tempfile::TempDir, path: &str) -> (StatusCode, String) {
            let response = router(dir)
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .expect("response");
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .expect("body");
            (status, String::from_utf8_lossy(&bytes).into_owned())
        }

        #[tokio::test]
        async fn the_index_is_served_at_the_root() {
            let dir = dist();
            let (status, body) = get(&dir, "/").await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains("<!doctype html>"), "{body}");
        }

        #[tokio::test]
        async fn real_assets_are_served() {
            let dir = dist();
            let (status, body) = get(&dir, "/assets/app.js").await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains("export const x"), "{body}");
        }

        /// A deep link must answer 200, not 404 with an HTML body. Client-side routing means
        /// the URL is handled by the app, so the request genuinely succeeded.
        #[tokio::test]
        async fn unknown_paths_fall_back_to_the_index_with_a_success_status() {
            let dir = dist();
            for path in ["/settings", "/meetings/abc123", "/deep/nested/route"] {
                let (status, body) = get(&dir, path).await;
                assert_eq!(status, StatusCode::OK, "{path} should be a 200");
                assert!(body.contains("<title>NW</title>"), "{path}: {body}");
            }
        }

        /// The fallback must not shadow the API, or a typo in a route would silently return
        /// HTML to a caller expecting JSON.
        #[tokio::test]
        async fn api_routes_win_over_the_frontend_fallback() {
            let dir = dist();
            let (status, body) = get(&dir, "/health").await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains("\"status\""), "{body}");
            assert!(!body.contains("<!doctype"), "html shadowed the api: {body}");
        }

        /// An unbuilt frontend is a real 404 with an actionable message, not a blank page.
        #[tokio::test]
        async fn a_missing_index_reports_how_to_fix_it() {
            let dir = tempfile::tempdir().expect("tempdir");
            let state = AppState::new(
                Database::open_in_memory().expect("db"),
                Router::from_config(RouterConfig::mock()).expect("router"),
            );
            let response = app_with_frontend(Arc::new(state), dir.path())
                .oneshot(Request::get("/").body(Body::empty()).unwrap())
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
                .await
                .unwrap();
            let body = String::from_utf8_lossy(&bytes);
            assert!(body.contains("npm run build"), "{body}");
        }
    }
}
