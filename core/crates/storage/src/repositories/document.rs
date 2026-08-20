//! Documents read from a watched folder, and vault files edited behind our back.
//!
//! # Why the body is stored
//!
//! A search result or a grounded answer that cites a document has to keep working when the file is
//! moved, renamed, or on a drive that is not mounted. Re-reading on demand would make every citation
//! contingent on the filesystem still looking the way it did.
//!
//! # Why a missing file is marked rather than deleted
//!
//! Deleting the row deletes its embeddings, so an answer that cited the document becomes uncitable
//! because somebody reorganised a folder. Marking it keeps the content and the citation while making
//! it clear the file is no longer where it was.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row};

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub id: Id,
    pub external_item_id: Id,
    pub path: String,
    pub title: String,
    pub body: String,
    pub byte_size: i64,
    pub modified_at: DateTime<Utc>,
    /// When the file stopped being found. `None` while it is present.
    pub missing_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewDocument {
    pub external_item_id: Id,
    pub path: String,
    pub title: String,
    pub body: String,
    pub byte_size: i64,
    pub modified_at: DateTime<Utc>,
}

/// How a divergence was settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Keep the file as the user left it and stop mirroring that meeting.
    Kept,
    /// Replace the file with the current render.
    Overwritten,
    /// Copy what the user wrote into a linked note, then resume mirroring.
    CopiedToNote,
}

impl Resolution {
    pub fn as_str(&self) -> &'static str {
        match self {
            Resolution::Kept => "kept",
            Resolution::Overwritten => "overwritten",
            Resolution::CopiedToNote => "copied_to_note",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "kept" => Resolution::Kept,
            "overwritten" => Resolution::Overwritten,
            "copied_to_note" => Resolution::CopiedToNote,
            _ => return None,
        })
    }
}

/// A vault file that was edited outside Notewise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub id: Id,
    pub external_item_id: Id,
    pub path: String,
    pub detected_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolution: Option<Resolution>,
}

#[derive(Debug)]
pub struct DocumentRepository<'a> {
    db: &'a Database,
}

impl<'a> DocumentRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Insert or refresh the document for an external item.
    ///
    /// Re-importing clears `missing_at`: a file that came back is present again, and leaving it
    /// marked would keep it out of search for no reason.
    pub fn upsert(&self, new: NewDocument) -> Result<Document> {
        let now = Utc::now();
        self.db.conn().execute(
            "INSERT INTO documents
                (id, external_item_id, path, title, body, byte_size, modified_at, missing_at,
                 updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8)
             ON CONFLICT(external_item_id) DO UPDATE SET
                path        = excluded.path,
                title       = excluded.title,
                body        = excluded.body,
                byte_size   = excluded.byte_size,
                modified_at = excluded.modified_at,
                missing_at  = NULL,
                updated_at  = excluded.updated_at",
            rusqlite::params![
                Id::new(),
                new.external_item_id,
                new.path,
                new.title,
                new.body,
                new.byte_size,
                new.modified_at,
                now,
            ],
        )?;

        self.by_external_item(new.external_item_id)?
            .ok_or_else(|| StorageError::not_found("Document", new.external_item_id))
    }

    pub fn by_external_item(&self, external_item_id: Id) -> Result<Option<Document>> {
        let conn = self.db.conn();
        let row = conn
            .query_row(
                &format!("{SELECT_DOC} WHERE external_item_id = ?1"),
                rusqlite::params![external_item_id],
                map_document,
            )
            .optional()?;
        Ok(row)
    }

    pub fn get(&self, id: Id) -> Result<Document> {
        let conn = self.db.conn();
        conn.query_row(
            &format!("{SELECT_DOC} WHERE id = ?1"),
            rusqlite::params![id],
            map_document,
        )
        .optional()?
        .ok_or_else(|| StorageError::not_found("Document", id))
    }

    /// Present documents, newest first.
    pub fn list(&self) -> Result<Vec<Document>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(&format!(
            "{SELECT_DOC} WHERE missing_at IS NULL ORDER BY modified_at DESC"
        ))?;
        let rows = stmt
            .query_map([], map_document)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every path currently known, so a scan can tell what has disappeared.
    pub fn known_paths(&self) -> Result<Vec<(Id, String)>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare("SELECT id, path FROM documents WHERE missing_at IS NULL")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Mark a document's file as no longer found.
    pub fn mark_missing(&self, id: Id) -> Result<()> {
        let changed = self.db.conn().execute(
            "UPDATE documents SET missing_at = ?2, updated_at = ?2 WHERE id = ?1",
            rusqlite::params![id, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Document", id));
        }
        Ok(())
    }

    // ------------------------------------------------------------------ divergences

    /// Record that a vault file was edited outside Notewise.
    ///
    /// One open record per file: a mirror that refuses on every attempt must not produce a row per
    /// attempt, or the list becomes unreadable within a day.
    pub fn record_divergence(&self, external_item_id: Id, path: &str) -> Result<Divergence> {
        self.db.conn().execute(
            "INSERT INTO vault_divergences (id, external_item_id, path, detected_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(external_item_id) DO UPDATE SET
                path = excluded.path
             WHERE vault_divergences.resolved_at IS NOT NULL",
            rusqlite::params![Id::new(), external_item_id, path, Utc::now()],
        )?;

        self.divergence_for(external_item_id)?
            .ok_or_else(|| StorageError::not_found("Divergence", external_item_id))
    }

    pub fn divergence_for(&self, external_item_id: Id) -> Result<Option<Divergence>> {
        let conn = self.db.conn();
        let row = conn
            .query_row(
                &format!("{SELECT_DIV} WHERE external_item_id = ?1"),
                rusqlite::params![external_item_id],
                map_divergence,
            )
            .optional()?;
        Ok(row)
    }

    /// Divergences the user has not decided about.
    pub fn open_divergences(&self) -> Result<Vec<Divergence>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(&format!(
            "{SELECT_DIV} WHERE resolved_at IS NULL ORDER BY detected_at"
        ))?;
        let rows = stmt
            .query_map([], map_divergence)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn resolve_divergence(&self, id: Id, resolution: Resolution) -> Result<Divergence> {
        let changed = self.db.conn().execute(
            "UPDATE vault_divergences SET resolved_at = ?2, resolution = ?3
              WHERE id = ?1 AND resolved_at IS NULL",
            rusqlite::params![id, Utc::now(), resolution.as_str()],
        )?;
        if changed == 0 {
            return Err(StorageError::Refused(
                "that divergence has already been settled".into(),
            ));
        }

        let conn = self.db.conn();
        conn.query_row(
            &format!("{SELECT_DIV} WHERE id = ?1"),
            rusqlite::params![id],
            map_divergence,
        )
        .optional()?
        .ok_or_else(|| StorageError::not_found("Divergence", id))
    }
}

const SELECT_DOC: &str = "SELECT id, external_item_id, path, title, body, byte_size, modified_at, \
     missing_at, updated_at FROM documents";

const SELECT_DIV: &str = "SELECT id, external_item_id, path, detected_at, resolved_at, resolution \
     FROM vault_divergences";

fn map_document(row: &Row<'_>) -> rusqlite::Result<Document> {
    Ok(Document {
        id: row.get(0)?,
        external_item_id: row.get(1)?,
        path: row.get(2)?,
        title: row.get(3)?,
        body: row.get(4)?,
        byte_size: row.get(5)?,
        modified_at: row.get(6)?,
        missing_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn map_divergence(row: &Row<'_>) -> rusqlite::Result<Divergence> {
    let raw: Option<String> = row.get(5)?;
    Ok(Divergence {
        id: row.get(0)?,
        external_item_id: row.get(1)?,
        path: row.get(2)?,
        detected_at: row.get(3)?,
        resolved_at: row.get(4)?,
        resolution: raw.as_deref().and_then(Resolution::parse),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::{ExternalItemRepository, NewExternalItem};

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn external(db: &Database, id: &str) -> Id {
        ExternalItemRepository::new(db)
            .upsert(NewExternalItem {
                connector_id: "documents".into(),
                external_id: id.into(),
                url: None,
                title: Some("Notes".into()),
                remote_version: None,
            })
            .expect("external item")
            .id
    }

    fn doc(item: Id, path: &str, body: &str) -> NewDocument {
        NewDocument {
            external_item_id: item,
            path: path.into(),
            title: "Architecture notes".into(),
            body: body.into(),
            byte_size: body.len() as i64,
            modified_at: Utc::now(),
        }
    }

    #[test]
    fn a_document_round_trips() {
        let db = db();
        let item = external(&db, "a");
        let repo = DocumentRepository::new(&db);

        let made = repo
            .upsert(doc(item, "/vault/a.md", "hello"))
            .expect("upsert");
        assert_eq!(repo.get(made.id).expect("get"), made);
        assert_eq!(repo.list().expect("list").len(), 1);
    }

    #[test]
    fn re_importing_updates_rather_than_duplicating() {
        let db = db();
        let item = external(&db, "a");
        let repo = DocumentRepository::new(&db);

        repo.upsert(doc(item, "/vault/a.md", "first"))
            .expect("first");
        let second = repo
            .upsert(doc(item, "/vault/a.md", "second"))
            .expect("second");

        assert_eq!(second.body, "second");
        assert_eq!(repo.list().expect("list").len(), 1);
    }

    /// The body is stored so a citation survives the file being moved or the drive unmounted.
    #[test]
    fn the_body_is_readable_without_the_file() {
        let db = db();
        let item = external(&db, "a");
        let repo = DocumentRepository::new(&db);
        let made = repo
            .upsert(doc(item, "/nowhere/that/exists.md", "the content"))
            .expect("upsert");

        assert_eq!(repo.get(made.id).expect("get").body, "the content");
    }

    /// Deleting the row would delete its embeddings and make a cited answer uncitable.
    #[test]
    fn a_missing_file_keeps_its_content_but_leaves_the_list() {
        let db = db();
        let item = external(&db, "a");
        let repo = DocumentRepository::new(&db);
        let made = repo
            .upsert(doc(item, "/vault/a.md", "hello"))
            .expect("upsert");

        repo.mark_missing(made.id).expect("mark");

        assert!(repo.list().expect("list").is_empty());
        let still = repo.get(made.id).expect("still there");
        assert_eq!(still.body, "hello");
        assert!(still.missing_at.is_some());
    }

    #[test]
    fn a_file_that_comes_back_is_present_again() {
        let db = db();
        let item = external(&db, "a");
        let repo = DocumentRepository::new(&db);
        let made = repo
            .upsert(doc(item, "/vault/a.md", "hello"))
            .expect("upsert");
        repo.mark_missing(made.id).expect("mark");

        repo.upsert(doc(item, "/vault/a.md", "hello again"))
            .expect("reimport");

        assert_eq!(repo.list().expect("list").len(), 1);
        assert!(repo.get(made.id).expect("get").missing_at.is_none());
    }

    #[test]
    fn known_paths_excludes_what_is_already_missing() {
        let db = db();
        let repo = DocumentRepository::new(&db);
        let a = repo
            .upsert(doc(external(&db, "a"), "/v/a.md", "x"))
            .expect("a");
        repo.upsert(doc(external(&db, "b"), "/v/b.md", "y"))
            .expect("b");
        repo.mark_missing(a.id).expect("mark");

        let paths: Vec<String> = repo
            .known_paths()
            .expect("paths")
            .into_iter()
            .map(|(_, p)| p)
            .collect();
        assert_eq!(paths, vec!["/v/b.md".to_string()]);
    }

    /// A mirror that refuses on every attempt must not produce a row per attempt.
    #[test]
    fn a_divergence_is_recorded_once_while_it_is_open() {
        let db = db();
        let item = external(&db, "a");
        let repo = DocumentRepository::new(&db);

        let first = repo.record_divergence(item, "/vault/m.md").expect("first");
        let again = repo.record_divergence(item, "/vault/m.md").expect("again");

        assert_eq!(first.id, again.id);
        assert_eq!(repo.open_divergences().expect("open").len(), 1);
    }

    #[test]
    fn a_resolved_divergence_leaves_the_open_list() {
        let db = db();
        let item = external(&db, "a");
        let repo = DocumentRepository::new(&db);
        let found = repo.record_divergence(item, "/vault/m.md").expect("record");

        let settled = repo
            .resolve_divergence(found.id, Resolution::CopiedToNote)
            .expect("resolve");

        assert_eq!(settled.resolution, Some(Resolution::CopiedToNote));
        assert!(settled.resolved_at.is_some());
        assert!(repo.open_divergences().expect("open").is_empty());
    }

    #[test]
    fn settling_the_same_divergence_twice_is_refused() {
        let db = db();
        let item = external(&db, "a");
        let repo = DocumentRepository::new(&db);
        let found = repo.record_divergence(item, "/vault/m.md").expect("record");
        repo.resolve_divergence(found.id, Resolution::Kept)
            .expect("first");

        assert!(repo
            .resolve_divergence(found.id, Resolution::Overwritten)
            .is_err());
    }

    /// Diverging again after a decision is a new decision to make, not a silent overwrite of the
    /// old one.
    #[test]
    fn a_file_can_diverge_again_after_being_settled() {
        let db = db();
        let item = external(&db, "a");
        let repo = DocumentRepository::new(&db);
        let found = repo.record_divergence(item, "/vault/m.md").expect("record");
        repo.resolve_divergence(found.id, Resolution::Overwritten)
            .expect("resolve");

        repo.record_divergence(item, "/vault/m.md").expect("again");
        // Still one row — the same file — but the record is refreshed for a new decision.
        assert!(repo.divergence_for(item).expect("found").is_some());
    }

    #[test]
    fn resolutions_round_trip() {
        for r in [
            Resolution::Kept,
            Resolution::Overwritten,
            Resolution::CopiedToNote,
        ] {
            assert_eq!(Resolution::parse(r.as_str()), Some(r));
        }
        assert_eq!(Resolution::parse("merged"), None);
    }

    #[test]
    fn documents_are_searchable_and_a_missing_one_is_not() {
        let db = db();
        let item = external(&db, "a");
        let repo = DocumentRepository::new(&db);
        let made = repo
            .upsert(doc(item, "/v/a.md", "the cost structure is unusual"))
            .expect("upsert");

        let hits: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM search_index WHERE search_index MATCH 'structure'",
                [],
                |r| r.get(0),
            )
            .expect("query");
        assert_eq!(hits, 1);

        repo.mark_missing(made.id).expect("mark");
        let after: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM search_index WHERE search_index MATCH 'structure'",
                [],
                |r| r.get(0),
            )
            .expect("query");
        assert_eq!(
            after, 0,
            "a file that is gone should stop answering questions"
        );
    }
}
