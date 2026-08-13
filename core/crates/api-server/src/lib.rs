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

mod error;
mod routes;
mod state;

pub use error::{ApiError, ApiResult};
pub use state::AppState;

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

    /// Bind to `127.0.0.1` on the default port.
    pub fn local() -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], Self::DEFAULT_PORT)),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Serve until the process is signalled.
    pub async fn serve(self, state: AppState) -> Result<(), ServeError> {
        let listener = tokio::net::TcpListener::bind(self.addr)
            .await
            .map_err(|source| ServeError::Bind {
                addr: self.addr,
                source,
            })?;

        let bound = listener.local_addr()?;
        tracing::info!(addr = %bound, "notewise api listening on loopback");

        axum::serve(listener, app(Arc::new(state)))
            .with_graceful_shutdown(shutdown_signal())
            .await?;

        Ok(())
    }
}

/// Build the route table.
///
/// Public so tests and embedders (the desktop app runs this in-process) can drive the API
/// without opening a socket.
pub fn app(state: Arc<AppState>) -> AxumRouter {
    routes::router(state)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
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
}
