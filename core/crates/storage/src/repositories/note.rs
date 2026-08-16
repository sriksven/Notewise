use chrono::Utc;
use rusqlite::Row;

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;
use crate::models::Note;

#[derive(Debug, Clone)]
pub struct NewNote {
    pub project_id: Option<Id>,
    pub title: String,
    /// Serialized blocks. Opaque here so the editor format can change without a migration.
    pub body: String,
}

#[derive(Debug)]
pub struct NoteRepository<'a> {
    db: &'a Database,
}

impl<'a> NoteRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn create(&self, new: NewNote) -> Result<Note> {
        let now = Utc::now();
        let note = Note {
            id: Id::new(),
            project_id: new.project_id,
            title: new.title,
            body: new.body,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        };

        self.db.conn().execute(
            "INSERT INTO notes (id, project_id, title, body, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                note.id,
                note.project_id,
                note.title,
                note.body,
                note.created_at,
                note.updated_at
            ],
        )?;

        Ok(note)
    }

    /// One note, trashed or not.
    ///
    /// Deliberately does not filter on `deleted_at`: the trash view previews what it is about
    /// to destroy, and restoring a note requires being able to read it first.
    pub fn get(&self, id: Id) -> Result<Note> {
        self.db
            .conn()
            .query_row(
                "SELECT id, project_id, title, body, created_at, updated_at, deleted_at
                 FROM notes WHERE id = ?1",
                rusqlite::params![id],
                map_note,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StorageError::not_found("Note", id),
                other => other.into(),
            })
    }

    /// Live notes, most recently touched first. Trashed notes are excluded.
    pub fn list_recent(&self, limit: u32) -> Result<Vec<Note>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, body, created_at, updated_at, deleted_at
             FROM notes WHERE deleted_at IS NULL ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit], map_note)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list_in_project(&self, project_id: Id) -> Result<Vec<Note>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, body, created_at, updated_at, deleted_at
             FROM notes
             WHERE project_id = ?1 AND deleted_at IS NULL
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_id], map_note)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// What is in the trash, most recently discarded first.
    pub fn list_trashed(&self) -> Result<Vec<Note>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, body, created_at, updated_at, deleted_at
             FROM notes WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC",
        )?;
        let rows = stmt.query_map([], map_note)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Edit a note.
    ///
    /// Refuses a trashed note rather than silently resurrecting it: an autosave timer firing
    /// after the user pressed delete would otherwise undo the delete, and the write would look
    /// like it succeeded.
    pub fn update(&self, id: Id, title: &str, body: &str) -> Result<Note> {
        let changed = self.db.conn().execute(
            "UPDATE notes SET title = ?2, body = ?3, updated_at = ?4
             WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![id, title, body, Utc::now()],
        )?;
        if changed == 0 {
            // Tell the two cases apart. "Not found" for a note sitting in the trash would send
            // a user looking for a bug in the wrong place.
            return Err(match self.get(id) {
                Ok(_) => StorageError::Invalid {
                    what: "note",
                    reason: "it is in the trash; restore it before editing".into(),
                },
                Err(e) => e,
            });
        }
        self.get(id)
    }

    /// Move a note to the trash. Reversible with [`Self::restore`].
    ///
    /// Idempotent in effect but not in timestamp: trashing an already-trashed note leaves the
    /// original discard time alone, so emptying the trash by age stays meaningful.
    pub fn trash(&self, id: Id) -> Result<Note> {
        self.db.conn().execute(
            "UPDATE notes SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![id, Utc::now()],
        )?;
        // No `changed == 0` check: zero rows means either missing or already trashed, and
        // `get` distinguishes them correctly.
        self.get(id)
    }

    pub fn restore(&self, id: Id) -> Result<Note> {
        self.db.conn().execute(
            "UPDATE notes SET deleted_at = NULL WHERE id = ?1",
            rusqlite::params![id],
        )?;
        self.get(id)
    }

    /// Destroy a note for good. The only path that reaches `DELETE`.
    ///
    /// Callers must detach the note's graph edges first — the edge table has no foreign keys
    /// and SQLite cannot cascade for it.
    pub fn purge(&self, id: Id) -> Result<()> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM notes WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::not_found("Note", id));
        }
        Ok(())
    }

    /// The ids currently in the trash, for a caller that needs to detach edges before
    /// [`Self::empty_trash`] destroys the rows.
    pub fn trashed_ids(&self) -> Result<Vec<Id>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare("SELECT id FROM notes WHERE deleted_at IS NOT NULL")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Destroy everything in the trash. Returns how many notes were removed.
    pub fn empty_trash(&self) -> Result<usize> {
        Ok(self
            .db
            .conn()
            .execute("DELETE FROM notes WHERE deleted_at IS NOT NULL", [])?)
    }
}

fn map_note(row: &Row<'_>) -> rusqlite::Result<Note> {
    Ok(Note {
        id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        body: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        deleted_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn note(db: &Database, title: &str) -> Note {
        NoteRepository::new(db)
            .create(NewNote {
                project_id: None,
                title: title.into(),
                body: "body text".into(),
            })
            .expect("create note")
    }

    #[test]
    fn round_trips_a_note() {
        let db = db();
        let created = note(&db, "Architecture");
        assert_eq!(NoteRepository::new(&db).get(created.id).unwrap(), created);
    }

    #[test]
    fn update_changes_content_and_bumps_timestamp() {
        let db = db();
        let repo = NoteRepository::new(&db);
        let created = note(&db, "Draft");

        let updated = repo.update(created.id, "Final", "revised body").unwrap();
        assert_eq!(updated.title, "Final");
        assert_eq!(updated.body, "revised body");
        assert!(updated.updated_at >= created.updated_at);
        assert_eq!(updated.created_at, created.created_at);
    }

    #[test]
    fn recent_notes_are_ordered_by_last_update() {
        let db = db();
        let repo = NoteRepository::new(&db);
        let first = note(&db, "First");
        let second = note(&db, "Second");

        // Touching the older note should move it to the front.
        repo.update(first.id, "First", "touched").unwrap();

        let titles: Vec<_> = repo
            .list_recent(10)
            .unwrap()
            .into_iter()
            .map(|n| n.title)
            .collect();
        assert_eq!(titles, vec!["First", "Second"]);
        assert_eq!(second.title, "Second");
    }

    #[test]
    fn purge_removes_the_note() {
        let db = db();
        let repo = NoteRepository::new(&db);
        let created = note(&db, "Temp");

        repo.purge(created.id).unwrap();
        assert!(matches!(
            repo.get(created.id).expect_err("deleted"),
            StorageError::NotFound { kind: "Note", .. }
        ));
    }

    #[test]
    fn updating_a_missing_note_reports_not_found() {
        let db = db();
        let err = NoteRepository::new(&db)
            .update(Id::new(), "x", "y")
            .expect_err("should be missing");
        assert!(matches!(err, StorageError::NotFound { kind: "Note", .. }));
    }

    #[test]
    fn trashing_hides_a_note_from_the_list_but_keeps_it_readable() {
        let db = db();
        let repo = NoteRepository::new(&db);
        let created = note(&db, "Draft");

        let trashed = repo.trash(created.id).unwrap();
        assert!(trashed.deleted_at.is_some());
        assert!(repo.list_recent(10).unwrap().is_empty());

        // Still readable, so the trash view can show what it holds.
        assert_eq!(repo.get(created.id).unwrap().title, "Draft");
        assert_eq!(repo.list_trashed().unwrap().len(), 1);
    }

    #[test]
    fn restoring_puts_a_note_back_with_its_content() {
        let db = db();
        let repo = NoteRepository::new(&db);
        let created = note(&db, "Recovered");

        repo.trash(created.id).unwrap();
        let restored = repo.restore(created.id).unwrap();

        assert_eq!(restored.deleted_at, None);
        assert_eq!(restored.body, "body text");
        assert_eq!(repo.list_recent(10).unwrap().len(), 1);
        assert!(repo.list_trashed().unwrap().is_empty());
    }

    /// The failure this guards against: the notes editor autosaves on a timer, so a save
    /// queued a moment before the user pressed delete would land *after* the delete. Silently
    /// clearing `deleted_at` there would resurrect a note the user had thrown away.
    #[test]
    fn a_trashed_note_cannot_be_edited_back_into_existence() {
        let db = db();
        let repo = NoteRepository::new(&db);
        let created = note(&db, "Gone");
        repo.trash(created.id).unwrap();

        let err = repo
            .update(created.id, "Back", "resurrected")
            .expect_err("editing a trashed note should fail");
        assert!(
            matches!(err, StorageError::Invalid { what: "note", .. }),
            "got {err:?}"
        );

        let still = repo.get(created.id).unwrap();
        assert!(still.deleted_at.is_some(), "note should still be trashed");
        assert_eq!(still.title, "Gone", "the edit should not have applied");
    }

    #[test]
    fn trashing_twice_keeps_the_first_discard_time() {
        let db = db();
        let repo = NoteRepository::new(&db);
        let created = note(&db, "Once");

        let first = repo.trash(created.id).unwrap();
        let again = repo.trash(created.id).unwrap();
        assert_eq!(first.deleted_at, again.deleted_at);
    }

    #[test]
    fn emptying_the_trash_destroys_only_trashed_notes() {
        let db = db();
        let repo = NoteRepository::new(&db);
        let kept = note(&db, "Kept");
        let discarded = note(&db, "Discarded");
        repo.trash(discarded.id).unwrap();

        assert_eq!(repo.empty_trash().unwrap(), 1);
        assert_eq!(repo.get(kept.id).unwrap().title, "Kept");
        assert!(repo.get(discarded.id).is_err());
    }

    #[test]
    fn trashed_ids_lists_what_empty_trash_would_destroy() {
        let db = db();
        let repo = NoteRepository::new(&db);
        let live = note(&db, "Live");
        let gone = note(&db, "Gone");
        repo.trash(gone.id).unwrap();

        assert_eq!(repo.trashed_ids().unwrap(), vec![gone.id]);
        assert_eq!(repo.get(live.id).unwrap().title, "Live");
    }

    /// A note in the trash must stop appearing in search. Trashing is an `UPDATE`, so this
    /// depends entirely on the v7 trigger doing a conditional re-insert.
    #[test]
    fn a_trashed_note_leaves_the_search_index_and_returns_on_restore() {
        use crate::repositories::search::SearchRepository;

        let db = db();
        let notes = NoteRepository::new(&db);
        let created = NoteRepository::new(&db)
            .create(NewNote {
                project_id: None,
                title: "Pricing".into(),
                body: "we agreed on a discount".into(),
            })
            .unwrap();

        let search = SearchRepository::new(&db);
        assert_eq!(search.search("discount", 10).unwrap().len(), 1);

        notes.trash(created.id).unwrap();
        assert!(
            search.search("discount", 10).unwrap().is_empty(),
            "a trashed note should not be findable"
        );

        notes.restore(created.id).unwrap();
        assert_eq!(
            search.search("discount", 10).unwrap().len(),
            1,
            "restoring should put it back in the index"
        );
    }
}
