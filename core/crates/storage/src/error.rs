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

    #[error("database schema version {found} is newer than this build supports ({supported}); upgrade Notewise")]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("invalid stored value in column '{column}': {reason}")]
    Corrupt {
        column: &'static str,
        reason: String,
    },

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("encryption is not available: this binary was built without the 'sqlcipher' feature")]
    EncryptionUnavailable,
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
