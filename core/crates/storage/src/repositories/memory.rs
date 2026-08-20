//! What the app remembers about the person using it.
//!
//! # Caps are refusals, not warnings
//!
//! A memory is injected into the system prompt of calls that already carry retrieved material and a
//! transcript. An unbounded list crowds out the actual content and makes every answer slightly
//! worse, in a way that is nearly impossible to attribute to its cause. So the cap is enforced here,
//! at the write, and reaching it is an error the caller has to resolve by deleting something.
//!
//! # Nothing here is on by default
//!
//! This crate stores; it does not decide to remember. Automatic extraction is gated by a setting
//! that ships off, and the manual path works whether or not that is ever turned on — adding a
//! memory by hand is a deliberate act and needs no feature switch.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row};

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;

/// Global memories apply to everything, so the ceiling is low.
pub const GLOBAL_CAP: usize = 5;

/// Project memories apply only within a project, so there is room for more.
pub const PROJECT_CAP: usize = 20;

/// Where a memory applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryScope {
    /// Everywhere.
    Global,
    /// Only in meetings belonging to one project.
    Project,
}

impl MemoryScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryScope::Global => "global",
            MemoryScope::Project => "project",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "global" => MemoryScope::Global,
            "project" => MemoryScope::Project,
            _ => return None,
        })
    }

    pub fn cap(&self) -> usize {
        match self {
            MemoryScope::Global => GLOBAL_CAP,
            MemoryScope::Project => PROJECT_CAP,
        }
    }
}

/// How a memory came to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryOrigin {
    /// The user typed it.
    Manual,
    /// A background pass proposed it and the reflector accepted it.
    Extracted,
}

impl MemoryOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryOrigin::Manual => "manual",
            MemoryOrigin::Extracted => "extracted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "manual" => MemoryOrigin::Manual,
            "extracted" => MemoryOrigin::Extracted,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    pub id: Id,
    pub scope: MemoryScope,
    pub project_id: Option<Id>,
    pub text: String,
    pub origin: MemoryOrigin,
    /// The meeting it came from, while that meeting still exists. Answers "why does it think that".
    pub source_meeting_id: Option<Id>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewMemory {
    pub scope: MemoryScope,
    /// Required for [`MemoryScope::Project`], forbidden for [`MemoryScope::Global`].
    pub project_id: Option<Id>,
    pub text: String,
    pub origin: MemoryOrigin,
    pub source_meeting_id: Option<Id>,
}

#[derive(Debug)]
pub struct MemoryRepository<'a> {
    db: &'a Database,
}

impl<'a> MemoryRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Add a memory, refusing if its scope is already full.
    pub fn create(&self, new: NewMemory) -> Result<Memory> {
        let text = new.text.trim();
        if text.is_empty() {
            return Err(StorageError::Refused(
                "a memory with no text remembers nothing".into(),
            ));
        }

        // Checked here as well as by the schema, so the caller gets a sentence rather than a
        // constraint-violation string it would have to interpret.
        match (new.scope, new.project_id) {
            (MemoryScope::Project, None) => {
                return Err(StorageError::Refused(
                    "a project memory needs a project".into(),
                ))
            }
            (MemoryScope::Global, Some(_)) => {
                return Err(StorageError::Refused(
                    "a global memory applies everywhere and cannot belong to a project".into(),
                ))
            }
            _ => {}
        }

        let used = self.count(new.scope, new.project_id)?;
        let cap = new.scope.cap();
        if used >= cap {
            return Err(StorageError::Refused(format!(
                "that is already {cap} {} memories, which is the limit. Delete one to make room.",
                new.scope.as_str()
            )));
        }

        let now = Utc::now();
        let memory = Memory {
            id: Id::new(),
            scope: new.scope,
            project_id: new.project_id,
            text: text.to_string(),
            origin: new.origin,
            source_meeting_id: new.source_meeting_id,
            created_at: now,
            updated_at: now,
        };

        self.db.conn().execute(
            "INSERT INTO memories
                (id, scope, project_id, text, origin, source_meeting_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            rusqlite::params![
                memory.id,
                memory.scope.as_str(),
                memory.project_id,
                memory.text,
                memory.origin.as_str(),
                memory.source_meeting_id,
                now,
            ],
        )?;

        Ok(memory)
    }

    /// How many memories a scope holds.
    pub fn count(&self, scope: MemoryScope, project_id: Option<Id>) -> Result<usize> {
        let conn = self.db.conn();
        let count: i64 = match scope {
            MemoryScope::Global => conn.query_row(
                "SELECT COUNT(*) FROM memories WHERE scope = 'global'",
                [],
                |r| r.get(0),
            )?,
            MemoryScope::Project => conn.query_row(
                "SELECT COUNT(*) FROM memories WHERE scope = 'project' AND project_id = ?1",
                rusqlite::params![project_id],
                |r| r.get(0),
            )?,
        };
        Ok(count as usize)
    }

    /// Every memory, newest first. For the screen where they are reviewed.
    pub fn list(&self) -> Result<Vec<Memory>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, scope, project_id, text, origin, source_meeting_id, created_at, updated_at
               FROM memories ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map([], map_memory)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// What applies to a meeting in `project_id`: every global memory, plus that project's.
    pub fn applicable(&self, project_id: Option<Id>) -> Result<Vec<Memory>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, scope, project_id, text, origin, source_meeting_id, created_at, updated_at
               FROM memories
              WHERE scope = 'global' OR (scope = 'project' AND project_id = ?1)
              ORDER BY scope, created_at",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![project_id], map_memory)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get(&self, id: Id) -> Result<Memory> {
        self.db
            .conn()
            .query_row(
                "SELECT id, scope, project_id, text, origin, source_meeting_id, created_at,
                        updated_at
                   FROM memories WHERE id = ?1",
                rusqlite::params![id],
                map_memory,
            )
            .optional()?
            .ok_or_else(|| StorageError::not_found("Memory", id))
    }

    /// Change a memory's text.
    ///
    /// Editing rather than only add-and-delete because a memory that is nearly right is the common
    /// case, and retyping it to fix a word invites deleting the wrong one.
    pub fn update(&self, id: Id, text: &str) -> Result<Memory> {
        let text = text.trim();
        if text.is_empty() {
            return Err(StorageError::Refused(
                "a memory with no text remembers nothing".into(),
            ));
        }

        let changed = self.db.conn().execute(
            "UPDATE memories SET text = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, text, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Memory", id));
        }
        self.get(id)
    }

    pub fn delete(&self, id: Id) -> Result<()> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM memories WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::not_found("Memory", id));
        }
        Ok(())
    }

    /// Meetings that have ended and have not been through extraction.
    ///
    /// Oldest first, so a backlog is worked through in the order things happened.
    pub fn unprocessed_meetings(&self, limit: usize) -> Result<Vec<Id>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT m.id FROM meetings m
              LEFT JOIN memory_extraction_state s ON s.meeting_id = m.id
              WHERE m.ended_at IS NOT NULL
                AND m.deleted_at IS NULL
                AND s.meeting_id IS NULL
              ORDER BY m.started_at
              LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Record that a meeting has been through extraction, whether or not it yielded anything.
    ///
    /// Marked either way on purpose: a meeting with nothing worth remembering must not be re-read on
    /// every pass forever, which would spend model calls to reach the same answer.
    pub fn mark_processed(&self, meeting_id: Id) -> Result<()> {
        self.db.conn().execute(
            "INSERT INTO memory_extraction_state (meeting_id, processed_at) VALUES (?1, ?2)
             ON CONFLICT(meeting_id) DO UPDATE SET processed_at = excluded.processed_at",
            rusqlite::params![meeting_id, Utc::now()],
        )?;
        Ok(())
    }
}

fn map_memory(row: &Row<'_>) -> rusqlite::Result<Memory> {
    let scope: String = row.get(1)?;
    let origin: String = row.get(4)?;
    Ok(Memory {
        id: row.get(0)?,
        scope: MemoryScope::parse(&scope).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                format!("unknown memory scope '{scope}'").into(),
            )
        })?,
        project_id: row.get(2)?,
        text: row.get(3)?,
        origin: MemoryOrigin::parse(&origin).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                format!("unknown memory origin '{origin}'").into(),
            )
        })?,
        source_meeting_id: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::{
        MeetingRepository, NewMeeting, NewProject, NewWorkspace, ProjectRepository,
        WorkspaceRepository,
    };
    use crate::MeetingSource;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn project(db: &Database) -> Id {
        let ws = WorkspaceRepository::new(db)
            .create(NewWorkspace {
                name: "Work".into(),
            })
            .expect("workspace");
        ProjectRepository::new(db)
            .create(NewProject {
                workspace_id: ws.id,
                name: "Platform".into(),
                description: None,
            })
            .expect("project")
            .id
    }

    fn global(text: &str) -> NewMemory {
        NewMemory {
            scope: MemoryScope::Global,
            project_id: None,
            text: text.into(),
            origin: MemoryOrigin::Manual,
            source_meeting_id: None,
        }
    }

    fn in_project(id: Id, text: &str) -> NewMemory {
        NewMemory {
            scope: MemoryScope::Project,
            project_id: Some(id),
            text: text.into(),
            origin: MemoryOrigin::Extracted,
            source_meeting_id: None,
        }
    }

    #[test]
    fn a_memory_round_trips() {
        let db = db();
        let repo = MemoryRepository::new(&db);
        let made = repo
            .create(global("I prefer short summaries"))
            .expect("create");

        assert_eq!(repo.get(made.id).expect("get"), made);
        assert_eq!(repo.list().expect("list").len(), 1);
    }

    #[test]
    fn nothing_is_remembered_until_something_is_added() {
        let db = db();
        assert!(MemoryRepository::new(&db).list().expect("list").is_empty());
    }

    /// The cap is a refusal with a sentence, not a silent drop and not a raw constraint error.
    #[test]
    fn the_global_cap_is_enforced() {
        let db = db();
        let repo = MemoryRepository::new(&db);
        for n in 0..GLOBAL_CAP {
            repo.create(global(&format!("fact {n}")))
                .expect("within cap");
        }

        let err = repo.create(global("one too many")).expect_err("over cap");
        let StorageError::Refused(message) = err else {
            panic!("expected a refusal, got {err:?}");
        };
        assert!(message.contains("Delete one"), "{message}");
    }

    #[test]
    fn the_project_cap_is_enforced_per_project() {
        let db = db();
        let repo = MemoryRepository::new(&db);
        let a = project(&db);
        let b = project(&db);

        for n in 0..PROJECT_CAP {
            repo.create(in_project(a, &format!("a{n}")))
                .expect("within cap");
        }
        assert!(repo.create(in_project(a, "over")).is_err());

        // A different project has its own budget.
        repo.create(in_project(b, "b0")).expect("other project");
    }

    #[test]
    fn deleting_one_makes_room_again() {
        let db = db();
        let repo = MemoryRepository::new(&db);
        let mut ids = Vec::new();
        for n in 0..GLOBAL_CAP {
            ids.push(
                repo.create(global(&format!("fact {n}")))
                    .expect("create")
                    .id,
            );
        }
        assert!(repo.create(global("over")).is_err());

        repo.delete(ids[0]).expect("delete");
        repo.create(global("now it fits")).expect("after delete");
    }

    /// The schema invariant, checked through the repository's own message rather than a raw
    /// constraint violation the caller would have to interpret.
    #[test]
    fn scope_and_project_must_agree() {
        let db = db();
        let repo = MemoryRepository::new(&db);
        let p = project(&db);

        let orphan = NewMemory {
            scope: MemoryScope::Project,
            project_id: None,
            text: "x".into(),
            origin: MemoryOrigin::Manual,
            source_meeting_id: None,
        };
        assert!(repo.create(orphan).is_err());

        let overreaching = NewMemory {
            scope: MemoryScope::Global,
            project_id: Some(p),
            text: "x".into(),
            origin: MemoryOrigin::Manual,
            source_meeting_id: None,
        };
        assert!(repo.create(overreaching).is_err());
    }

    #[test]
    fn an_empty_memory_is_refused() {
        let db = db();
        let repo = MemoryRepository::new(&db);
        assert!(repo.create(global("   ")).is_err());

        let made = repo.create(global("real")).expect("create");
        assert!(repo.update(made.id, " ").is_err());
    }

    #[test]
    fn what_applies_is_every_global_plus_this_projects() {
        let db = db();
        let repo = MemoryRepository::new(&db);
        let a = project(&db);
        let b = project(&db);

        repo.create(global("everywhere")).expect("global");
        repo.create(in_project(a, "only a")).expect("a");
        repo.create(in_project(b, "only b")).expect("b");

        let for_a: Vec<String> = repo
            .applicable(Some(a))
            .expect("applicable")
            .into_iter()
            .map(|m| m.text)
            .collect();

        assert!(for_a.contains(&"everywhere".to_string()));
        assert!(for_a.contains(&"only a".to_string()));
        assert!(
            !for_a.contains(&"only b".to_string()),
            "another project's memories must not leak in: {for_a:?}"
        );
    }

    #[test]
    fn a_meeting_with_no_project_still_gets_the_global_ones() {
        let db = db();
        let repo = MemoryRepository::new(&db);
        repo.create(global("everywhere")).expect("global");

        assert_eq!(repo.applicable(None).expect("applicable").len(), 1);
    }

    #[test]
    fn a_memory_can_be_edited() {
        let db = db();
        let repo = MemoryRepository::new(&db);
        let made = repo
            .create(global("I prefer shot summaries"))
            .expect("create");

        let fixed = repo
            .update(made.id, "I prefer short summaries")
            .expect("update");
        assert_eq!(fixed.text, "I prefer short summaries");
        assert_eq!(fixed.id, made.id, "editing must not replace it");
    }

    #[test]
    fn deleting_a_project_takes_its_memories_with_it() {
        let db = db();
        let repo = MemoryRepository::new(&db);
        let p = project(&db);
        repo.create(in_project(p, "scoped")).expect("create");

        ProjectRepository::new(&db)
            .delete(p)
            .expect("delete project");
        assert!(repo.list().expect("list").is_empty());
    }

    #[test]
    fn extraction_only_offers_ended_meetings_and_only_once() {
        let db = db();
        let repo = MemoryRepository::new(&db);
        let meetings = MeetingRepository::new(&db);

        let open = meetings
            .create(NewMeeting {
                project_id: None,
                title: "Still going".into(),
                source: MeetingSource::Microphone,
                started_at: Utc::now(),
            })
            .expect("open meeting");
        let done = meetings
            .create(NewMeeting {
                project_id: None,
                title: "Finished".into(),
                source: MeetingSource::Microphone,
                started_at: Utc::now(),
            })
            .expect("meeting");
        meetings.end(done.id, Utc::now()).expect("end");

        let pending = repo.unprocessed_meetings(10).expect("pending");
        assert_eq!(
            pending,
            vec![done.id],
            "a live meeting is not ready to learn from"
        );
        assert!(!pending.contains(&open.id));

        repo.mark_processed(done.id).expect("mark");
        assert!(
            repo.unprocessed_meetings(10).expect("pending").is_empty(),
            "a meeting with nothing worth remembering must not be re-read forever"
        );
    }

    #[test]
    fn marking_the_same_meeting_twice_is_harmless() {
        let db = db();
        let repo = MemoryRepository::new(&db);
        let meetings = MeetingRepository::new(&db);
        let m = meetings
            .create(NewMeeting {
                project_id: None,
                title: "Finished".into(),
                source: MeetingSource::Microphone,
                started_at: Utc::now(),
            })
            .expect("meeting");
        meetings.end(m.id, Utc::now()).expect("end");

        repo.mark_processed(m.id).expect("first");
        repo.mark_processed(m.id).expect("second");
    }

    #[test]
    fn scopes_and_origins_round_trip() {
        for scope in [MemoryScope::Global, MemoryScope::Project] {
            assert_eq!(MemoryScope::parse(scope.as_str()), Some(scope));
        }
        for origin in [MemoryOrigin::Manual, MemoryOrigin::Extracted] {
            assert_eq!(MemoryOrigin::parse(origin.as_str()), Some(origin));
        }
        assert_eq!(MemoryScope::parse("everywhere"), None);
        assert_eq!(MemoryOrigin::parse("guessed"), None);
    }
}
