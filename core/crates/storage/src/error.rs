use std::path::PathBuf;

use thiserror::Error;

/// Errors produced by the storage layer.
///
/// Surfaces translate these at their boundary — `api-server` maps them to HTTP status
/// codes, `cli` to exit codes — so this enum carries no transport concerns.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Sqlite(rusqlite::Error),

    /// A write broke a rule the schema enforces: a value that must be unique already is, or an id
    /// that has to name a row names nothing.
    ///
    /// Split out of [`Self::Sqlite`] because it is the caller's doing rather than a fault. As
    /// `Sqlite` these reached the API as 500s — a note filed under a project that does not exist, a
    /// second person with the same email — telling the client to retry a request that fails
    /// identically every time. Every rusqlite error passes through the `From` impl below, so this
    /// holds for each repository without each one having to remember.
    #[error("{message}")]
    Constraint {
        kind: ConstraintKind,
        message: String,
    },

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

    /// The operation is valid SQL and deliberately not allowed.
    ///
    /// Separate from a constraint failure because the reason is a product decision rather than a
    /// schema one, and the message explains the rule rather than naming a table.
    #[error("{0}")]
    Refused(String),

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

/// Which schema rule a [`StorageError::Constraint`] broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintKind {
    /// UNIQUE or PRIMARY KEY: something with that value already exists.
    Unique,
    /// FOREIGN KEY: the row points at something that does not exist, or something still points at
    /// the row being removed.
    ForeignKey,
    /// CHECK, NOT NULL and the rest: a value the schema does not accept.
    Other,
}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        let rusqlite::Error::SqliteFailure(failure, detail) = &error else {
            return StorageError::Sqlite(error);
        };
        if failure.code != rusqlite::ErrorCode::ConstraintViolation {
            return StorageError::Sqlite(error);
        }

        // SQLite's own wording names the table and column ("UNIQUE constraint failed:
        // people.email"), which is the useful part and carries no user content.
        let detail = detail.clone().unwrap_or_default();
        let (kind, message) = match failure.extended_code {
            rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE | rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY => {
                let target = detail.rsplit(": ").next().unwrap_or("that value");
                (
                    ConstraintKind::Unique,
                    format!("something with the same {target} already exists"),
                )
            }
            // SQLite does not say which reference failed, so neither can this.
            rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY => (
                ConstraintKind::ForeignKey,
                "this refers to something that does not exist, or something else still refers to it"
                    .to_string(),
            ),
            _ => (ConstraintKind::Other, detail),
        };

        StorageError::Constraint { kind, message }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Database, MeetingRepository, MeetingSource, NewMeeting, NewNote, NoteRepository};

    /// A row pointing at a parent that does not exist is the caller's mistake, and has to arrive
    /// as one. As a bare `Sqlite` error it became a 500 at the API.
    #[test]
    fn a_missing_parent_is_a_constraint_error_not_a_database_fault() {
        let db = Database::open_in_memory().expect("db");
        let err = NoteRepository::new(&db)
            .create(NewNote {
                project_id: Some(crate::Id::new()),
                title: "orphan".into(),
                body: String::new(),
            })
            .expect_err("the project does not exist");

        assert!(
            matches!(
                err,
                StorageError::Constraint {
                    kind: ConstraintKind::ForeignKey,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn a_duplicate_names_what_collided() {
        let db = Database::open_in_memory().expect("db");
        let meeting = MeetingRepository::new(&db)
            .create(NewMeeting {
                project_id: None,
                title: "t".into(),
                source: MeetingSource::Import,
                started_at: chrono::Utc::now(),
            })
            .expect("meeting");

        // The primary key is the one unique constraint every table has.
        let err: StorageError = db
            .conn()
            .execute(
                "INSERT INTO meetings (id, title, source, started_at, created_at, updated_at)
                 VALUES (?1, 't', 'import', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z',
                         '2026-01-01T00:00:00Z')",
                rusqlite::params![meeting.id],
            )
            .expect_err("duplicate id")
            .into();

        match err {
            StorageError::Constraint {
                kind: ConstraintKind::Unique,
                message,
            } => assert!(message.contains("meetings.id"), "{message}"),
            other => panic!("expected a unique-constraint error, got {other:?}"),
        }
    }

    /// Everything that is not a constraint stays what it was.
    #[test]
    fn other_failures_stay_database_errors() {
        let err: StorageError = rusqlite::Error::QueryReturnedNoRows.into();
        assert!(matches!(err, StorageError::Sqlite(_)), "{err:?}");
    }
}
