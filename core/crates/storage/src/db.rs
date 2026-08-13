use std::path::Path;

use rusqlite::Connection;

use crate::error::{Result, StorageError};
use crate::migrations;

/// An open local database, migrated to the current schema version.
///
/// This is the only type in Notewise that owns a SQLite connection. Everything outside
/// this crate goes through repositories.
pub struct Database {
    conn: Connection,
}

// `rusqlite::Connection` is not `Debug`, so this is written by hand. Deliberately reports
// only the schema version — a connection handle has nothing else worth printing, and
// anything derived from user data must not leak into logs.
impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field(
                "schema_version",
                &self.schema_version().map_err(|_| std::fmt::Error)?,
            )
            .finish()
    }
}

impl Database {
    /// Open (or create) a database file and bring it to the current schema version.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::from_connection(conn, true)
    }

    /// Open an in-memory database. Used by tests and by `--ephemeral` CLI runs.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn, false)
    }

    /// Open an encrypted database.
    ///
    /// Requires the `sqlcipher` feature. Without it this returns
    /// [`StorageError::EncryptionUnavailable`] rather than silently opening the database
    /// unencrypted — failing loudly is the only safe behaviour when a caller asked for
    /// encryption and cannot have it.
    #[cfg_attr(not(feature = "sqlcipher"), allow(unused_variables))]
    pub fn open_encrypted(path: impl AsRef<Path>, key: &str) -> Result<Self> {
        #[cfg(feature = "sqlcipher")]
        {
            let conn = Connection::open(path)?;
            // Must be the first statement executed on the connection.
            conn.pragma_update(None, "key", key)?;
            Self::from_connection(conn, true)
        }
        #[cfg(not(feature = "sqlcipher"))]
        {
            Err(StorageError::EncryptionUnavailable)
        }
    }

    fn from_connection(conn: Connection, persistent: bool) -> Result<Self> {
        // Cascading deletes are part of the schema's correctness, and SQLite leaves this
        // off by default. It must be set per-connection, not once per database.
        conn.pragma_update(None, "foreign_keys", "ON")?;

        if persistent {
            // WAL lets the recording path write while the UI reads. Meaningless for
            // in-memory databases, which is why it is gated.
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.busy_timeout(std::time::Duration::from_secs(5))?;
        }

        let mut db = Database { conn };
        migrations::migrate(&mut db.conn)?;
        Ok(db)
    }

    /// Schema version of this database.
    pub fn schema_version(&self) -> Result<u32> {
        migrations::current_version(&self.conn)
    }

    /// Borrow the underlying connection.
    ///
    /// Crate-internal on purpose: repositories use this, callers outside `storage` do not
    /// get raw SQL access. See the dependency rules in ARCHITECTURE.md.
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Run a closure inside a transaction, rolling back if it returns `Err`.
    pub fn transaction<T, F>(&mut self, f: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    {
        let tx = self.conn.transaction()?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_database_is_migrated_on_open() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), migrations::SUPPORTED_VERSION);
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let db = Database::open_in_memory().unwrap();
        let enabled: i32 = db
            .conn()
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(enabled, 1, "cascade deletes depend on this being on");
    }

    #[test]
    fn transaction_rolls_back_on_error() {
        let mut db = Database::open_in_memory().unwrap();

        let result: Result<()> = db.transaction(|tx| {
            tx.execute(
                "INSERT INTO workspaces (id, name, created_at, updated_at)
                 VALUES ('w', 'doomed', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                [],
            )?;
            Err(StorageError::not_found("Workspace", "forced failure"))
        });
        assert!(result.is_err());

        let count: u32 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM workspaces", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "failed transaction must not leave rows behind");
    }

    #[test]
    fn transaction_commits_on_success() {
        let mut db = Database::open_in_memory().unwrap();

        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO workspaces (id, name, created_at, updated_at)
                 VALUES ('w', 'kept', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let count: u32 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM workspaces", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    #[cfg(not(feature = "sqlcipher"))]
    fn encryption_fails_loudly_when_not_compiled_in() {
        let err = Database::open_encrypted("/tmp/notewise-should-not-exist.db", "hunter2")
            .expect_err("must not silently open unencrypted");
        assert!(matches!(err, StorageError::EncryptionUnavailable));
    }
}
