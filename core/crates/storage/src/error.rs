use std::path::PathBuf;

use thiserror::Error;

/// Errors produced by the storage layer.
///
/// Surfaces translate these at their boundary — `api-server` maps them to HTTP status
/// codes, `cli` to exit codes — so this enum carries no transport concerns.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("{kind} not found: {id}")]
    NotFound { kind: &'static str, id: String },

    #[error("migration failed at version {version}: {source}")]
    Migration {
        version: u32,
        #[source]
        source: rusqlite::Error,
    },

    /// A merge could not proceed. Distinct from [`StorageError::Sqlite`] because every one of
    /// these is a condition the user can act on — a missing file, a schema mismatch, the same
    /// workspace twice — and the message says which.
    #[error("cannot merge: {0}")]
    Merge(String),

    #[error("database schema version {found} is newer than this build supports ({supported}); upgrade Notewise")]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("invalid stored value in column '{column}': {reason}")]
    Corrupt {
        column: &'static str,
        reason: String,
    },

    /// A caller supplied fields that contradict each other. Distinct from [`Self::Corrupt`],
    /// which is about what is already stored: this one is rejected before it is written.
    #[error("invalid {what}: {reason}")]
    Invalid { what: &'static str, reason: String },

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("encryption is not available: this binary was built without the 'sqlcipher' feature")]
    EncryptionUnavailable,

    /// No home directory, and no `NOTEWISE_DATA_DIR` to stand in for one. Named rather than
    /// guessed: a workspace written to a fallback path is a workspace the user cannot find.
    #[error("could not determine where to keep the workspace; set NOTEWISE_DATA_DIR or pass an explicit database path")]
    NoDataDirectory,

    #[error("could not prepare the workspace directory {path}: {source}")]
    DataDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not move the workspace from {from} to {to}: {source}")]
    WorkspaceMove {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, StorageError>;

impl StorageError {
    /// Convenience for repository lookups that must return exactly one row.
    pub fn not_found(kind: &'static str, id: impl std::fmt::Display) -> Self {
        StorageError::NotFound {
            kind,
            id: id.to_string(),
        }
    }
}
