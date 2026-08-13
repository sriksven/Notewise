use chrono::{DateTime, Utc};
use rusqlite::Row;

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;
use crate::models::{ActionItem, Decision, Summary, WorkStatus};

use super::decode_enum;

#[derive(Debug, Clone)]
pub struct NewSummary {
    pub meeting_id: Id,
    pub text: String,
    /// Model that produced this, so a summary can be regenerated or audited later.
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct NewDecision {
    pub summary_id: Id,
    pub text: String,
    pub reasoning: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewActionItem {
    pub summary_id: Id,
    pub text: String,
    pub owner: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub struct SummaryRepository<'a> {
    db: &'a Database,
}

impl<'a> SummaryRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn create(&self, new: NewSummary) -> Result<Summary> {
        let summary = Summary {
            id: Id::new(),
            meeting_id: new.meeting_id,
            text: new.text,
            model: new.model,
            created_at: Utc::now(),
        };

        self.db.conn().execute(
            "INSERT INTO summaries (id, meeting_id, text, model, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                summary.id,
                summary.meeting_id,
                summary.text,
                summary.model,
                summary.created_at
            ],
        )?;

        Ok(summary)
    }

    pub fn get(&self, id: Id) -> Result<Summary> {
        self.db
            .conn()
            .query_row(
                "SELECT id, meeting_id, text, model, created_at FROM summaries WHERE id = ?1",
                rusqlite::params![id],
                map_summary,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StorageError::not_found("Summary", id),
                other => other.into(),
            })
    }

    /// All summaries for a meeting, newest first. A meeting can have more than one when a
    /// summary is regenerated with a different model.
    pub fn list_for_meeting(&self, meeting_id: Id) -> Result<Vec<Summary>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, meeting_id, text, model, created_at
             FROM summaries WHERE meeting_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![meeting_id], map_summary)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Most recent summary for a meeting, if any.
    pub fn latest_for_meeting(&self, meeting_id: Id) -> Result<Option<Summary>> {
        Ok(self.list_for_meeting(meeting_id)?.into_iter().next())
    }

    pub fn add_decision(&self, new: NewDecision) -> Result<Decision> {
        let decision = Decision {
            id: Id::new(),
            summary_id: new.summary_id,
            text: new.text,
            reasoning: new.reasoning,
            decided_at: new.decided_at,
        };

        self.db.conn().execute(
            "INSERT INTO decisions (id, summary_id, text, reasoning, decided_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                decision.id,
                decision.summary_id,
                decision.text,
                decision.reasoning,
                decision.decided_at
            ],
        )?;

        Ok(decision)
    }

    pub fn decisions(&self, summary_id: Id) -> Result<Vec<Decision>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, summary_id, text, reasoning, decided_at
             FROM decisions WHERE summary_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![summary_id], map_decision)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn add_action_item(&self, new: NewActionItem) -> Result<ActionItem> {
        let now = Utc::now();
        let item = ActionItem {
            id: Id::new(),
            summary_id: new.summary_id,
            text: new.text,
            owner: new.owner,
            due_at: new.due_at,
            status: WorkStatus::Todo,
            created_at: now,
            updated_at: now,
        };

        self.db.conn().execute(
            "INSERT INTO action_items
                (id, summary_id, text, owner, due_at, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                item.id,
                item.summary_id,
                item.text,
                item.owner,
                item.due_at,
                item.status.as_str(),
                item.created_at,
                item.updated_at
            ],
        )?;

        Ok(item)
    }

    pub fn action_items(&self, summary_id: Id) -> Result<Vec<ActionItem>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, summary_id, text, owner, due_at, status, created_at, updated_at
             FROM action_items WHERE summary_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![summary_id], map_action_item)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().collect()
    }

    /// Open action items past their due date. Drives overdue notifications.
    pub fn overdue(&self, now: DateTime<Utc>) -> Result<Vec<ActionItem>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, summary_id, text, owner, due_at, status, created_at, updated_at
             FROM action_items
             WHERE due_at IS NOT NULL AND due_at < ?1 AND status IN ('todo', 'in_progress')
             ORDER BY due_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![now], map_action_item)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().collect()
    }

    pub fn set_action_item_status(&self, id: Id, status: WorkStatus) -> Result<()> {
        let changed = self.db.conn().execute(
            "UPDATE action_items SET status = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, status.as_str(), Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("ActionItem", id));
        }
        Ok(())
    }

    pub fn assign_action_item(&self, id: Id, owner: Option<&str>) -> Result<()> {
        let changed = self.db.conn().execute(
            "UPDATE action_items SET owner = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, owner, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("ActionItem", id));
        }
        Ok(())
    }
}

fn map_summary(row: &Row<'_>) -> rusqlite::Result<Summary> {
    Ok(Summary {
        id: row.get(0)?,
        meeting_id: row.get(1)?,
        text: row.get(2)?,
        model: row.get(3)?,
        created_at: row.get(4)?,
    })
}

fn map_decision(row: &Row<'_>) -> rusqlite::Result<Decision> {
    Ok(Decision {
        id: row.get(0)?,
        summary_id: row.get(1)?,
        text: row.get(2)?,
        reasoning: row.get(3)?,
        decided_at: row.get(4)?,
    })
}

fn map_action_item(row: &Row<'_>) -> rusqlite::Result<Result<ActionItem>> {
    let status_raw: String = row.get(5)?;
    Ok((|| {
        Ok(ActionItem {
            id: row.get(0)?,
            summary_id: row.get(1)?,
            text: row.get(2)?,
            owner: row.get(3)?,
            due_at: row.get(4)?,
            status: decode_enum("action_items.status", &status_raw, WorkStatus::parse)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::MeetingSource;
    use crate::repositories::{MeetingRepository, NewMeeting};
    use chrono::TimeZone;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).single().expect("valid timestamp")
    }

    fn setup() -> (Database, Summary) {
        let db = Database::open_in_memory().expect("in-memory db");
        let meeting = MeetingRepository::new(&db)
            .create(NewMeeting {
                project_id: None,
                title: "Planning".into(),
                source: MeetingSource::Microphone,
                started_at: ts(1_700_000_000),
            })
            .unwrap();
        let summary = SummaryRepository::new(&db)
            .create(NewSummary {
                meeting_id: meeting.id,
                text: "We agreed to ship Friday.".into(),
                model: "mock".into(),
            })
            .unwrap();
        (db, summary)
    }

    #[test]
    fn round_trips_a_summary() {
        let (db, summary) = setup();
        assert_eq!(SummaryRepository::new(&db).get(summary.id).unwrap(), summary);
    }

    #[test]
    fn latest_for_meeting_returns_newest_summary() {
        let (db, first) = setup();
        let repo = SummaryRepository::new(&db);

        let second = repo
            .create(NewSummary {
                meeting_id: first.meeting_id,
                text: "Regenerated with a better model.".into(),
                model: "llama3".into(),
            })
            .unwrap();

        let latest = repo.latest_for_meeting(first.meeting_id).unwrap().unwrap();
        assert_eq!(latest.id, second.id);
        assert_eq!(repo.list_for_meeting(first.meeting_id).unwrap().len(), 2);
    }

    #[test]
    fn latest_for_meeting_is_none_when_unsummarized() {
        let (db, _) = setup();
        let orphan = MeetingRepository::new(&db)
            .create(NewMeeting {
                project_id: None,
                title: "Unsummarized".into(),
                source: MeetingSource::Import,
                started_at: ts(1_700_000_000),
            })
            .unwrap();
        assert!(SummaryRepository::new(&db)
            .latest_for_meeting(orphan.id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn decisions_persist_with_reasoning() {
        let (db, summary) = setup();
        let repo = SummaryRepository::new(&db);

        repo.add_decision(NewDecision {
            summary_id: summary.id,
            text: "Ship Friday".into(),
            reasoning: Some("QA signed off".into()),
            decided_at: Some(ts(1_700_000_500)),
        })
        .unwrap();

        let decisions = repo.decisions(summary.id).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].reasoning.as_deref(), Some("QA signed off"));
    }

    #[test]
    fn action_items_start_as_todo() {
        let (db, summary) = setup();
        let repo = SummaryRepository::new(&db);

        let item = repo
            .add_action_item(NewActionItem {
                summary_id: summary.id,
                text: "Write the release notes".into(),
                owner: None,
                due_at: None,
            })
            .unwrap();

        assert_eq!(item.status, WorkStatus::Todo);
        assert!(item.status.is_open());
    }

    #[test]
    fn status_transitions_persist() {
        let (db, summary) = setup();
        let repo = SummaryRepository::new(&db);
        let item = repo
            .add_action_item(NewActionItem {
                summary_id: summary.id,
                text: "Write the release notes".into(),
                owner: None,
                due_at: None,
            })
            .unwrap();

        repo.set_action_item_status(item.id, WorkStatus::Done).unwrap();

        let stored = &repo.action_items(summary.id).unwrap()[0];
        assert_eq!(stored.status, WorkStatus::Done);
        assert!(!stored.status.is_open());
    }

    #[test]
    fn overdue_excludes_future_undated_and_closed_items() {
        let (db, summary) = setup();
        let repo = SummaryRepository::new(&db);
        let now = ts(1_700_000_000);

        let past_due = repo
            .add_action_item(NewActionItem {
                summary_id: summary.id,
                text: "Overdue".into(),
                owner: Some("alex".into()),
                due_at: Some(ts(1_699_000_000)),
            })
            .unwrap();
        repo.add_action_item(NewActionItem {
            summary_id: summary.id,
            text: "Not yet due".into(),
            owner: None,
            due_at: Some(ts(1_800_000_000)),
        })
        .unwrap();
        repo.add_action_item(NewActionItem {
            summary_id: summary.id,
            text: "No due date".into(),
            owner: None,
            due_at: None,
        })
        .unwrap();
        let finished = repo
            .add_action_item(NewActionItem {
                summary_id: summary.id,
                text: "Overdue but done".into(),
                owner: None,
                due_at: Some(ts(1_699_000_000)),
            })
            .unwrap();
        repo.set_action_item_status(finished.id, WorkStatus::Done)
            .unwrap();

        let overdue = repo.overdue(now).unwrap();
        assert_eq!(overdue.len(), 1, "got {overdue:?}");
        assert_eq!(overdue[0].id, past_due.id);
    }

    #[test]
    fn assigning_an_owner_persists() {
        let (db, summary) = setup();
        let repo = SummaryRepository::new(&db);
        let item = repo
            .add_action_item(NewActionItem {
                summary_id: summary.id,
                text: "Unowned".into(),
                owner: None,
                due_at: None,
            })
            .unwrap();

        repo.assign_action_item(item.id, Some("jordan")).unwrap();
        assert_eq!(
            repo.action_items(summary.id).unwrap()[0].owner.as_deref(),
            Some("jordan")
        );
    }

    #[test]
    fn deleting_a_meeting_cascades_through_summary_to_decisions() {
        let (db, summary) = setup();
        let repo = SummaryRepository::new(&db);
        repo.add_decision(NewDecision {
            summary_id: summary.id,
            text: "Doomed".into(),
            reasoning: None,
            decided_at: None,
        })
        .unwrap();

        MeetingRepository::new(&db).delete(summary.meeting_id).unwrap();

        assert!(repo.get(summary.id).is_err());
        assert_eq!(repo.decisions(summary.id).unwrap().len(), 0);
    }

    #[test]
    fn missing_action_item_status_update_reports_not_found() {
        let (db, _) = setup();
        let err = SummaryRepository::new(&db)
            .set_action_item_status(Id::new(), WorkStatus::Done)
            .expect_err("should be missing");
        assert!(matches!(err, StorageError::NotFound { kind: "ActionItem", .. }));
    }
}
