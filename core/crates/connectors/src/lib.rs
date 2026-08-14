//! The connector seam.
//!
//! Every external service — a markdown vault, a webhook receiver, a calendar, a ticket
//! tracker — reaches Notewise through the traits in this crate. Nothing above it imports a
//! vendor SDK, for the same reason nothing above `notewise-ai-router` imports a model SDK.
//!
//! # Direction is a type, not a flag
//!
//! A calendar only pulls; a webhook only pushes. Rather than one trait whose methods are
//! half-unimplemented per connector, capability is expressed by which trait is implemented:
//! [`SourceConnector`] for inbound, [`SinkConnector`] for outbound, both over [`Connector`].
//!
//! # Delivery goes through the outbox
//!
//! Sinks are never called directly at the site that changed the data. Work is enqueued to
//! `connector_outbox` with an idempotency key and drained by [`Dispatcher`]. That is what
//! makes a retry update a ticket rather than file a second one.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod connector;
mod credentials;
mod dispatcher;
mod error;
mod keychain;
mod registry;
mod sinks;
mod types;

pub use connector::{Connector, SinkConnector, SourceConnector};
pub use credentials::{CredentialStore, MemoryStore, Secret};
pub use dispatcher::{DispatchReport, Dispatcher, RetryPolicy};
pub use error::{ConnectorError, Result};
pub use keychain::KeychainStore;
pub use registry::ConnectorRegistry;
// `sinks` also grows WebhookSink (Task 13), which stays pending until that task lands.
pub use sinks::{MockConnector, VaultSink};
pub use types::{Cursor, ExternalRef, Health, Inbound, Operation, Outbound, PullBatch};
