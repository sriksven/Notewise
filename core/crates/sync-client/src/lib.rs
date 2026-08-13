//! Opt-in cloud sync.
//!
//! **Inert unless the user turns sync on.** It is a separate crate specifically so a
//! local-only build never compiles it in — "your meetings stay on your machine" should be
//! true of the binary, not just of its configuration.
//!
//! # What is implemented here
//!
//! The versioning and conflict-resolution logic is complete and tested — the part that is
//! genuinely hard and where correctness matters, since a wrong merge silently destroys a
//! user's edit. There is **no network transport**: that needs a running sync service, which
//! is a Phase 2 concern (see docs/roadmap.md).
//!
//! # The model
//!
//! Each record carries a [`Version`]: a per-device counter plus the id of the device that
//! last wrote it. Comparing two versions answers "did one descend from the other, or did they
//! diverge?" — which is the question a merge actually needs, and one a wall-clock timestamp
//! cannot answer reliably across devices with skewed clocks.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod resolve;
mod version;

pub use resolve::{ConflictPolicy, Resolution, SyncEngine};
pub use version::{DeviceId, Ordering, Version};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("sync is not enabled")]
    NotEnabled,

    #[error("no transport is configured; cloud sync arrives in Phase 2")]
    NoTransport,

    #[error("record '{0}' has no version and cannot be merged")]
    Unversioned(String),
}

pub type Result<T> = std::result::Result<T, SyncError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_missing_transport_error_names_the_phase() {
        // The error a caller sees today should explain the state of the world.
        assert!(SyncError::NoTransport.to_string().contains("Phase 2"));
    }
}
