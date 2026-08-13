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

    pub fn get(&self, id: Id) -> Result<Note> {
        self.db
            .conn()
            .query_row(
                "SELECT id, project_id, title, body, created_at, updated_at
                 FROM notes WHERE id = ?1",
                rusqlite::params![id],
                map_note,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StorageError::not_found("Note", id),
                other => other.into(),
            })
    }

    pub fn list_recent(&self, limit: u32) -> Result<Vec<Note>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, body, created_at, updated_at
             FROM notes ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit], map_note)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list_in_project(&self, project_id: Id) -> Result<Vec<Note>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, body, created_at, updated_at
             FROM notes WHERE project_id = ?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_id], map_note)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn update(&self, id: Id, title: &str, body: &str) -> Result<Note> {
        let changed = self.db.conn().execute(
            "UPDATE notes SET title = ?2, body = ?3, updated_at = ?4 WHERE id = ?1",
            rusqlite::params![id, title, body, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Note", id));
        }
        self.get(id)
    }

    pub fn delete(&self, id: Id) -> Result<()> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM notes WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::not_found("Note", id));
        }
        Ok(())
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
    fn delete_removes_the_note() {
        let db = db();
        let repo = NoteRepository::new(&db);
        let created = note(&db, "Temp");

        repo.delete(created.id).unwrap();
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
}
