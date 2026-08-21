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

mod config;
mod connector;
mod credentials;
mod dispatcher;
mod error;
mod importer;
mod keychain;
mod registry;
mod sinks;
mod sources;
mod types;

pub use config::{build_registry, generate_signing_secret, SIGNING_KEY};
pub use connector::{Connector, SinkConnector, SourceConnector};
pub use credentials::{CredentialStore, MemoryStore, Secret};
pub use dispatcher::{DispatchReport, Dispatcher, RetryPolicy};
pub use error::{ConnectorError, Result};
pub use importer::{ImportReport, Importer};
pub use keychain::KeychainStore;
pub use registry::ConnectorRegistry;
pub use sinks::{vault_fingerprint, MockConnector, VaultSink, WebhookSink, SIGNATURE_HEADER};
pub use sources::{
    authorize_url, base64url, is_readable, join_url_of, parse_graph_time, scan as scan_folder,
    title_of, to_inbound, Calendar, Documents, DraftRef, Found, GoogleBridge, GraphAttendee,
    GraphEvent, GraphTime, MicrosoftGraph, Pkce, ScriptEvent, ScriptGuest, CLIENT_ID_KEY,
    DEPLOYMENT_URL_KEY, EXTENSIONS, MAX_BYTES, MAX_DEPTH, MAX_FILES, REFRESH_TOKEN_KEY,
    REQUIRED_VERSION, SCOPES as MICROSOFT_SCOPES, SHARED_KEY as GOOGLE_SHARED_KEY,
    WINDOW_BACK_DAYS, WINDOW_FORWARD_DAYS,
};
pub use types::{Cursor, ExternalRef, Health, Inbound, Operation, Outbound, PullBatch};
