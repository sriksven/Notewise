//! Records of artifacts that live in another system.
//!
//! An external item is owned by nothing in Notewise — it is a record of a thing elsewhere.
//! So it carries no owning foreign key; association to a meeting or action item is a
//! `synced_to` edge in the `graph` crate. Identity is `(connector_id, external_id)`, which
//! is what makes re-pushing update a ticket rather than file a second one.

use chrono::{DateTime, Utc};
use rusqlite::Row;

use crate::db::Database;
use crate::error::Result;
use crate::id::Id;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalItem {
    pub id: Id,
    pub connector_id: String,
    pub external_id: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub remote_version: Option<String>,
    pub last_synced_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewExternalItem {
    pub connector_id: String,
    pub external_id: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub remote_version: Option<String>,
}

#[derive(Debug)]
pub struct ExternalItemRepository<'a> {
    db: &'a Database,
}

impl<'a> ExternalItemRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Insert, or update the existing row for this `(connector_id, external_id)`.
    pub fn upsert(&self, new: NewExternalItem) -> Result<ExternalItem> {
        let now = Utc::now();
        // A fresh id unconditionally: `DO UPDATE SET` below never assigns `id`, so on
        // conflict SQLite keeps the existing row's own id and discards this one. Looking the
        // row up first to reuse its id would cost a third round trip and change nothing.
        let id = Id::new();

        self.db.conn().execute(
            "INSERT INTO external_items
                (id, connector_id, external_id, url, title, remote_version,
                 last_synced_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(connector_id, external_id) DO UPDATE SET
                url            = excluded.url,
                title          = excluded.title,
                remote_version = excluded.remote_version,
                last_synced_at = excluded.last_synced_at",
            rusqlite::params![
                id,
                new.connector_id,
                new.external_id,
                new.url,
                new.title,
                new.remote_version,
                now
            ],
        )?;

        self.find(&new.connector_id, &new.external_id)?
            .ok_or_else(|| crate::error::StorageError::not_found("external_item", new.external_id))
    }

    pub fn find(&self, connector_id: &str, external_id: &str) -> Result<Option<ExternalItem>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, connector_id, external_id, url, title, remote_version,
                    last_synced_at, created_at
             FROM external_items
             WHERE connector_id = ?1 AND external_id = ?2",
        )?;

        let mut rows = stmt.query(rusqlite::params![connector_id, external_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(read_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn get(&self, id: Id) -> Result<ExternalItem> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, connector_id, external_id, url, title, remote_version,
                    last_synced_at, created_at
             FROM external_items WHERE id = ?1",
        )?;

        let mut rows = stmt.query(rusqlite::params![id])?;
        match rows.next()? {
            Some(row) => read_row(row),
            None => Err(crate::error::StorageError::not_found("external_item", id)),
        }
    }
}

fn read_row(row: &Row<'_>) -> Result<ExternalItem> {
    Ok(ExternalItem {
        id: row.get(0)?,
        connector_id: row.get(1)?,
        external_id: row.get(2)?,
        url: row.get(3)?,
        title: row.get(4)?,
        remote_version: row.get(5)?,
        last_synced_at: row.get(6)?,
        created_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory database")
    }

    fn sample() -> NewExternalItem {
        NewExternalItem {
            connector_id: "linear".into(),
            external_id: "ENG-412".into(),
            url: Some("https://linear.app/x/issue/ENG-412".into()),
            title: Some("Fix login".into()),
            remote_version: Some("1".into()),
        }
    }

    #[test]
    fn upsert_creates_then_updates_in_place() {
        let db = db();
        let repo = ExternalItemRepository::new(&db);

        let first = repo.upsert(sample()).unwrap();
        assert_eq!(first.external_id, "ENG-412");

        let second = repo
            .upsert(NewExternalItem {
                title: Some("Fix login redirect".into()),
                remote_version: Some("2".into()),
                ..sample()
            })
            .unwrap();

        assert_eq!(second.id, first.id, "upsert must not create a second row");
        assert_eq!(second.title.as_deref(), Some("Fix login redirect"));
        assert_eq!(second.remote_version.as_deref(), Some("2"));
    }

    #[test]
    fn same_external_id_on_a_different_connector_is_a_different_item() {
        let db = db();
        let repo = ExternalItemRepository::new(&db);

        let linear = repo.upsert(sample()).unwrap();
        let jira = repo
            .upsert(NewExternalItem {
                connector_id: "jira".into(),
                ..sample()
            })
            .unwrap();

        assert_ne!(linear.id, jira.id);
    }

    #[test]
    fn find_returns_none_for_unknown_identity() {
        let db = db();
        let repo = ExternalItemRepository::new(&db);
        assert!(repo.find("linear", "ENG-999").unwrap().is_none());
    }
}
