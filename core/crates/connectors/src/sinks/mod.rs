//! Sink implementations.

mod mock;
mod vault;
mod webhook;

pub use mock::MockConnector;
pub use vault::{fingerprint as vault_fingerprint, VaultSink};
pub use webhook::{WebhookSink, SIGNATURE_HEADER};
