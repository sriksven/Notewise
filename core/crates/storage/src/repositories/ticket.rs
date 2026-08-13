use chrono::{DateTime, Utc};
use rusqlite::Row;

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;
use crate::models::{ExternalRef, Ticket, WorkStatus};

use super::decode_enum;

#[derive(Debug, Clone)]
pub struct NewTicket {
    pub project_id: Option<Id>,
    pub title: String,
    pub description: Option<String>,
    pub owner: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub struct TicketRepository<'a> {
    db: &'a Database,
}

impl<'a> TicketRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn create(&self, new: NewTicket) -> Result<Ticket> {
        let now = Utc::now();
        let ticket = Ticket {
            id: Id::new(),
            project_id: new.project_id,
            title: new.title,
            description: new.description,
            status: WorkStatus::Todo,
            owner: new.owner,
            due_at: new.due_at,
            external: None,
            created_at: now,
            updated_at: now,
        };

        self.db.conn().execute(
            "INSERT INTO tickets
                (id, project_id, title, description, status, owner, due_at,
                 external_provider, external_id, external_url, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, NULL, ?8, ?9)",
            rusqlite::params![
                ticket.id,
                ticket.project_id,
                ticket.title,
                ticket.description,
                ticket.status.as_str(),
                ticket.owner,
                ticket.due_at,
                ticket.created_at,
                ticket.updated_at
            ],
        )?;

        Ok(ticket)
    }

    pub fn get(&self, id: Id) -> Result<Ticket> {
        self.db
            .conn()
            .query_row(SELECT, rusqlite::params![id], map_ticket)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StorageError::not_found("Ticket", id),
                other => other.into(),
            })
            .and_then(|r| r)
    }

    pub fn list_open(&self) -> Result<Vec<Ticket>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, description, status, owner, due_at,
                    external_provider, external_id, external_url, created_at, updated_at
             FROM tickets WHERE status IN ('todo', 'in_progress')
             ORDER BY due_at IS NULL, due_at",
        )?;
        let rows = stmt.query_map([], map_ticket)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    pub fn list_in_project(&self, project_id: Id) -> Result<Vec<Ticket>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, description, status, owner, due_at,
                    external_provider, external_id, external_url, created_at, updated_at
             FROM tickets WHERE project_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_id], map_ticket)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    pub fn set_status(&self, id: Id, status: WorkStatus) -> Result<Ticket> {
        let changed = self.db.conn().execute(
            "UPDATE tickets SET status = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, status.as_str(), Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Ticket", id));
        }
        self.get(id)
    }

    /// Record that this ticket now mirrors an issue in an external tracker.
    ///
    /// Phase 2 ships one-way push only — this records where a ticket was pushed to, it
    /// does not imply the external side syncs back. See docs/roadmap.md.
    pub fn link_external(&self, id: Id, external: ExternalRef) -> Result<Ticket> {
        let changed = self.db.conn().execute(
            "UPDATE tickets
             SET external_provider = ?2, external_id = ?3, external_url = ?4, updated_at = ?5
             WHERE id = ?1",
            rusqlite::params![
                id,
                external.provider,
                external.external_id,
                external.url,
                Utc::now()
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Ticket", id));
        }
        self.get(id)
    }

    /// Find a ticket by its external tracker identity, so a webhook can locate the local row.
    pub fn find_by_external(&self, provider: &str, external_id: &str) -> Result<Option<Ticket>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, description, status, owner, due_at,
                    external_provider, external_id, external_url, created_at, updated_at
             FROM tickets WHERE external_provider = ?1 AND external_id = ?2",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![provider, external_id], map_ticket)?;
        match rows.next() {
            Some(row) => Ok(Some(row??)),
            None => Ok(None),
        }
    }

    pub fn delete(&self, id: Id) -> Result<()> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM tickets WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::not_found("Ticket", id));
        }
        Ok(())
    }
}

const SELECT: &str = "SELECT id, project_id, title, description, status, owner, due_at,
            external_provider, external_id, external_url, created_at, updated_at
     FROM tickets WHERE id = ?1";

fn map_ticket(row: &Row<'_>) -> rusqlite::Result<Result<Ticket>> {
    let status_raw: String = row.get(4)?;
    let provider: Option<String> = row.get(7)?;
    let external_id: Option<String> = row.get(8)?;
    let url: Option<String> = row.get(9)?;

    Ok((|| {
        Ok(Ticket {
            id: row.get(0)?,
            project_id: row.get(1)?,
            title: row.get(2)?,
            description: row.get(3)?,
            status: decode_enum("tickets.status", &status_raw, WorkStatus::parse)?,
            owner: row.get(5)?,
            due_at: row.get(6)?,
            // Provider and id are written together, so either both are set or neither is.
            external: match (provider, external_id) {
                (Some(provider), Some(external_id)) => Some(ExternalRef {
                    provider,
                    external_id,
                    url,
                }),
                _ => None,
            },
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn ticket(db: &Database, title: &str) -> Ticket {
        TicketRepository::new(db)
            .create(NewTicket {
                project_id: None,
                title: title.into(),
                description: None,
                owner: None,
                due_at: None,
            })
            .expect("create ticket")
    }

    #[test]
    fn new_tickets_start_as_todo_and_unlinked() {
        let db = db();
        let t = ticket(&db, "Fix the build");
        assert_eq!(t.status, WorkStatus::Todo);
        assert!(t.external.is_none());
    }

    #[test]
    fn round_trips_a_ticket() {
        let db = db();
        let created = ticket(&db, "Fix the build");
        assert_eq!(TicketRepository::new(&db).get(created.id).unwrap(), created);
    }

    #[test]
    fn list_open_excludes_closed_tickets() {
        let db = db();
        let repo = TicketRepository::new(&db);
        let open = ticket(&db, "Open");
        let done = ticket(&db, "Done");
        let cancelled = ticket(&db, "Cancelled");
        repo.set_status(done.id, WorkStatus::Done).unwrap();
        repo.set_status(cancelled.id, WorkStatus::Cancelled)
            .unwrap();

        let ids: Vec<_> = repo
            .list_open()
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, vec![open.id]);
    }

    #[test]
    fn external_link_round_trips() {
        let db = db();
        let repo = TicketRepository::new(&db);
        let t = ticket(&db, "Ship it");

        let linked = repo
            .link_external(
                t.id,
                ExternalRef {
                    provider: "linear".into(),
                    external_id: "ENG-421".into(),
                    url: Some("https://linear.app/x/ENG-421".into()),
                },
            )
            .unwrap();

        let external = linked.external.expect("should be linked");
        assert_eq!(external.provider, "linear");
        assert_eq!(external.external_id, "ENG-421");
    }

    #[test]
    fn find_by_external_locates_the_local_ticket() {
        let db = db();
        let repo = TicketRepository::new(&db);
        let t = ticket(&db, "Ship it");
        repo.link_external(
            t.id,
            ExternalRef {
                provider: "jira".into(),
                external_id: "PROJ-7".into(),
                url: None,
            },
        )
        .unwrap();

        let found = repo.find_by_external("jira", "PROJ-7").unwrap();
        assert_eq!(found.map(|t| t.id), Some(t.id));
        assert!(repo.find_by_external("jira", "PROJ-999").unwrap().is_none());
    }

    #[test]
    fn the_same_external_issue_cannot_be_linked_twice() {
        let db = db();
        let repo = TicketRepository::new(&db);
        let first = ticket(&db, "First");
        let second = ticket(&db, "Second");

        let external = ExternalRef {
            provider: "linear".into(),
            external_id: "ENG-1".into(),
            url: None,
        };
        repo.link_external(first.id, external.clone()).unwrap();

        let err = repo
            .link_external(second.id, external)
            .expect_err("duplicate external link must be rejected");
        assert!(matches!(err, StorageError::Sqlite(_)), "got {err:?}");
    }

    #[test]
    fn many_tickets_may_remain_unlinked() {
        // The unique index on external identity is partial; null provider must not collide.
        let db = db();
        ticket(&db, "One");
        ticket(&db, "Two");
        ticket(&db, "Three");
        assert_eq!(TicketRepository::new(&db).list_open().unwrap().len(), 3);
    }
}
