//! Local storage for Notewise.
//!
//! This crate owns the SQLite schema, migrations, and encryption at rest. It is the only
//! place in the codebase that writes SQL — everything else goes through repositories.
//!
//! # Ownership vs association
//!
//! Columns here model **ownership**: a transcript segment belongs to exactly one meeting.
//! **Association** — a note referencing a meeting, an action item linked to a ticket — is
//! modelled as typed edges by the `graph` crate, which builds on [`EdgeRepository`].
//!
//! # Example
//!
//! ```
//! use notewise_storage::{Database, NewWorkspace, WorkspaceRepository};
//!
//! let db = Database::open_in_memory()?;
//! let workspaces = WorkspaceRepository::new(&db);
//! let acme = workspaces.create(NewWorkspace { name: "Acme".into() })?;
//!
//! assert_eq!(workspaces.get(acme.id)?.name, "Acme");
//! # Ok::<(), notewise_storage::StorageError>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod db;
mod error;
mod export;
mod id;
mod location;
mod migrations;
mod models;
mod repositories;

pub use db::Database;
pub use error::{Result, StorageError};
pub use export::{meeting_to_markdown, ExportOptions};
pub use id::Id;
pub use location::{
    adopt_legacy_models, adopt_legacy_workspace, data_dir, database_path, legacy_database_paths,
    model_dir, Adoption, DATA_DIR_ENV, DB_FILE,
};
pub use migrations::SUPPORTED_VERSION;
pub use models::*;
pub use repositories::*;
