//! The traits every external service hides behind.

use async_trait::async_trait;

use crate::error::Result;
use crate::types::{Cursor, ExternalRef, Health, Outbound, PullBatch};

/// What every connector has, regardless of direction.
///
/// `Send + Sync` so one instance can be shared across the async runtime — the dispatcher
/// holds a registry and drains several connectors concurrently.
#[async_trait]
pub trait Connector: Send + Sync + std::fmt::Debug {
    /// Stable identifier, e.g. `"vault"`, `"webhook"`, `"google_calendar"`.
    ///
    /// Persisted in `connector_outbox` and `external_items`, so changing one is a breaking
    /// change to on-disk data.
    fn id(&self) -> &str;

    fn display_name(&self) -> &str;

    /// Whether this connector keeps data on the user's machine.
    ///
    /// Surfaced in the UI so "local only" is something a user can verify rather than trust.
    fn is_local(&self) -> bool;

    async fn health(&self) -> Result<Health>;
}

/// A connector that reads from a remote system.
#[async_trait]
pub trait SourceConnector: Connector {
    async fn pull(&self, since: Cursor) -> Result<PullBatch>;
}

/// A connector that writes to a remote system.
#[async_trait]
pub trait SinkConnector: Connector {
    /// Deliver one unit of work. Implementations must treat `outbound.existing`
    /// as "this was already pushed — update it" rather than creating a second artifact.
    async fn push(&self, outbound: &Outbound) -> Result<ExternalRef>;
}
