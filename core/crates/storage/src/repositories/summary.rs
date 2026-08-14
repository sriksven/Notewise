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
    pub meeting_id: Id,
    /// The summary that surfaced this, if one did. `None` for a decision a user recorded
    /// directly.
    pub summary_id: Option<Id>,
    pub text: String,
    pub reasoning: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}

impl NewDecision {
    /// A decision extracted from a summary, taking its meeting from the summary itself so
    /// the two cannot disagree.
    pub fn from_summary(summary: &Summary, text: impl Into<String>) -> Self {
        Self {
            meeting_id: summary.meeting_id,
            summary_id: Some(summary.id),
            text: text.into(),
            reasoning: None,
            decided_at: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewActionItem {
    pub meeting_id: Id,
    /// The summary that surfaced this, if one did. `None` for an item a user added by hand.
    pub summary_id: Option<Id>,
    pub text: String,
    pub owner: Option<String>,
    pub owner_person_id: Option<Id>,
    pub due_at: Option<DateTime<Utc>>,
}

impl NewActionItem {
    /// An action item extracted from a summary, taking its meeting from the summary itself
    /// so the two cannot disagree.
    pub fn from_summary(summary: &Summary, text: impl Into<String>) -> Self {
        Self {
            meeting_id: summary.meeting_id,
            summary_id: Some(summary.id),
            text: text.into(),
            owner: None,
            owner_person_id: None,
            due_at: None,
        }
    }

    /// An action item a user added straight onto a meeting, with no summary behind it.
    pub fn on_meeting(meeting_id: Id, text: impl Into<String>) -> Self {
        Self {
            meeting_id,
            summary_id: None,
            text: text.into(),
            owner: None,
            owner_person_id: None,
            due_at: None,
        }
    }
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

    /// Delete a summary, leaving the decisions and action items it proposed in place.
    ///
    /// Since v6 those carry their own `meeting_id` and only reference this row as
    /// provenance, so they survive with `summary_id` set to NULL. Before v6 this call would
    /// have destroyed them — which is why summarisation appends rather than replaces, and
    /// why that workaround is no longer needed.
    pub fn delete(&self, id: Id) -> Result<()> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM summaries WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::not_found("Summary", id));
        }
        Ok(())
    }

    pub fn add_decision(&self, new: NewDecision) -> Result<Decision> {
        self.reject_mismatched_summary(new.meeting_id, new.summary_id)?;

        let decision = Decision {
            id: Id::new(),
            meeting_id: new.meeting_id,
            summary_id: new.summary_id,
            text: new.text,
            reasoning: new.reasoning,
            decided_at: new.decided_at,
        };

        self.db.conn().execute(
            "INSERT INTO decisions (id, meeting_id, summary_id, text, reasoning, decided_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                decision.id,
                decision.meeting_id,
                decision.summary_id,
                decision.text,
                decision.reasoning,
                decision.decided_at
            ],
        )?;

        Ok(decision)
    }

    /// Decisions a given summary surfaced.
    ///
    /// Note this is narrower than [`Self::decisions_for_meeting`]: a decision whose summary
    /// has since been regenerated has no `summary_id` and appears only in the meeting-scoped
    /// query. Callers rendering "this meeting's decisions" want that one.
    pub fn decisions(&self, summary_id: Id) -> Result<Vec<Decision>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, meeting_id, summary_id, text, reasoning, decided_at
             FROM decisions WHERE summary_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![summary_id], map_decision)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every decision made in a meeting, whichever summary first surfaced it.
    pub fn decisions_for_meeting(&self, meeting_id: Id) -> Result<Vec<Decision>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, meeting_id, summary_id, text, reasoning, decided_at
             FROM decisions WHERE meeting_id = ?1 ORDER BY decided_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![meeting_id], map_decision)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn add_action_item(&self, new: NewActionItem) -> Result<ActionItem> {
        self.reject_mismatched_summary(new.meeting_id, new.summary_id)?;

        let now = Utc::now();
        let item = ActionItem {
            id: Id::new(),
            meeting_id: new.meeting_id,
            summary_id: new.summary_id,
            text: new.text,
            owner: new.owner,
            owner_person_id: new.owner_person_id,
            due_at: new.due_at,
            status: WorkStatus::Todo,
            created_at: now,
            updated_at: now,
        };

        self.db.conn().execute(
            "INSERT INTO action_items
                (id, meeting_id, summary_id, text, owner, owner_person_id, due_at, status,
                 created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                item.id,
                item.meeting_id,
                item.summary_id,
                item.text,
                item.owner,
                item.owner_person_id,
                item.due_at,
                item.status.as_str(),
                item.created_at,
                item.updated_at
            ],
        )?;

        Ok(item)
    }

    /// Action items a given summary surfaced. See the note on [`Self::decisions`] — for
    /// "this meeting's action items", use [`Self::action_items_for_meeting`].
    pub fn action_items(&self, summary_id: Id) -> Result<Vec<ActionItem>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, meeting_id, summary_id, text, owner, owner_person_id, due_at, status,
                    created_at, updated_at
             FROM action_items WHERE summary_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![summary_id], map_action_item)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    /// Every action item from a meeting, whichever summary first surfaced it and including
    /// ones a user added by hand.
    pub fn action_items_for_meeting(&self, meeting_id: Id) -> Result<Vec<ActionItem>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, meeting_id, summary_id, text, owner, owner_person_id, due_at, status,
                    created_at, updated_at
             FROM action_items WHERE meeting_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![meeting_id], map_action_item)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    /// Still-open action items from a meeting. The unit of follow-through: what a later
    /// meeting in the same series needs to ask about.
    pub fn open_action_items_for_meeting(&self, meeting_id: Id) -> Result<Vec<ActionItem>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, meeting_id, summary_id, text, owner, owner_person_id, due_at, status,
                    created_at, updated_at
             FROM action_items
             WHERE meeting_id = ?1 AND status IN ('todo', 'in_progress')
             ORDER BY due_at IS NULL, due_at, created_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![meeting_id], map_action_item)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    /// Open action items past their due date. Drives overdue notifications.
    pub fn overdue(&self, now: DateTime<Utc>) -> Result<Vec<ActionItem>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, meeting_id, summary_id, text, owner, owner_person_id, due_at, status,
                    created_at, updated_at
             FROM action_items
             WHERE due_at IS NOT NULL AND due_at < ?1 AND status IN ('todo', 'in_progress')
             ORDER BY due_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![now], map_action_item)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    /// Refuse a work item whose summary belongs to a different meeting.
    ///
    /// `meeting_id` and `summary_id` are both caller-supplied, so they can disagree. Left
    /// unchecked that writes a decision into the wrong meeting's history — a silent
    /// corruption that only surfaces much later, when someone reads the wrong meeting.
    fn reject_mismatched_summary(&self, meeting_id: Id, summary_id: Option<Id>) -> Result<()> {
        let Some(summary_id) = summary_id else {
            return Ok(());
        };
        let actual = self.get(summary_id)?.meeting_id;
        if actual != meeting_id {
            return Err(StorageError::Invalid {
                what: "action item or decision",
                reason: format!(
                    "summary {summary_id} belongs to meeting {actual}, not {meeting_id}"
                ),
            });
        }
        Ok(())
    }

    /// One action item by id.
    pub fn action_item(&self, id: Id) -> Result<ActionItem> {
        self.db
            .conn()
            .query_row(
                "SELECT id, meeting_id, summary_id, text, owner, owner_person_id, due_at, status,
                        created_at, updated_at
                 FROM action_items WHERE id = ?1",
                rusqlite::params![id],
                map_action_item,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StorageError::not_found("ActionItem", id),
                other => other.into(),
            })
            .and_then(|r| r)
    }

    /// Set or clear an action item's due date. `None` clears it.
    pub fn set_action_item_due(&self, id: Id, due_at: Option<DateTime<Utc>>) -> Result<()> {
        let changed = self.db.conn().execute(
            "UPDATE action_items SET due_at = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, due_at, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("ActionItem", id));
        }
        Ok(())
    }

    /// Point an action item at a known person, or clear the link with `None`.
    ///
    /// Leaves the free-text `owner` alone. The two are independent on purpose: a transcript
    /// may name someone this install has no row for, and resolving that later should not
    /// erase what was actually said.
    pub fn set_action_item_person(&self, id: Id, person_id: Option<Id>) -> Result<()> {
        let changed = self.db.conn().execute(
            "UPDATE action_items SET owner_person_id = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, person_id, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("ActionItem", id));
        }
        Ok(())
    }

    pub fn delete_action_item(&self, id: Id) -> Result<()> {
        let changed = self.db.conn().execute(
            "DELETE FROM action_items WHERE id = ?1",
            rusqlite::params![id],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("ActionItem", id));
        }
        Ok(())
    }

    pub fn delete_decision(&self, id: Id) -> Result<()> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM decisions WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::not_found("Decision", id));
        }
        Ok(())
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
        meeting_id: row.get(1)?,
        summary_id: row.get(2)?,
        text: row.get(3)?,
        reasoning: row.get(4)?,
        decided_at: row.get(5)?,
    })
}

pub(super) fn map_action_item(row: &Row<'_>) -> rusqlite::Result<Result<ActionItem>> {
    let status_raw: String = row.get(7)?;
    Ok((|| {
        Ok(ActionItem {
            id: row.get(0)?,
            meeting_id: row.get(1)?,
            summary_id: row.get(2)?,
            text: row.get(3)?,
            owner: row.get(4)?,
            owner_person_id: row.get(5)?,
            due_at: row.get(6)?,
            status: decode_enum("action_items.status", &status_raw, WorkStatus::parse)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
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
        Utc.timestamp_opt(secs, 0)
            .single()
            .expect("valid timestamp")
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
        assert_eq!(
            SummaryRepository::new(&db).get(summary.id).unwrap(),
            summary
        );
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
            reasoning: Some("QA signed off".into()),
            decided_at: Some(ts(1_700_000_500)),
            ..NewDecision::from_summary(&summary, "Ship Friday")
        })
        .unwrap();

        let decisions = repo.decisions(summary.id).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].reasoning.as_deref(), Some("QA signed off"));
        assert_eq!(decisions[0].meeting_id, summary.meeting_id);
    }

    #[test]
    fn action_items_start_as_todo() {
        let (db, summary) = setup();
        let repo = SummaryRepository::new(&db);

        let item = repo
            .add_action_item(NewActionItem::from_summary(
                &summary,
                "Write the release notes",
            ))
            .unwrap();

        assert_eq!(item.status, WorkStatus::Todo);
        assert!(item.status.is_open());
    }

    #[test]
    fn status_transitions_persist() {
        let (db, summary) = setup();
        let repo = SummaryRepository::new(&db);
        let item = repo
            .add_action_item(NewActionItem::from_summary(
                &summary,
                "Write the release notes",
            ))
            .unwrap();

        repo.set_action_item_status(item.id, WorkStatus::Done)
            .unwrap();

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
                owner: Some("alex".into()),
                due_at: Some(ts(1_699_000_000)),
                ..NewActionItem::from_summary(&summary, "Overdue")
            })
            .unwrap();
        repo.add_action_item(NewActionItem {
            due_at: Some(ts(1_800_000_000)),
            ..NewActionItem::from_summary(&summary, "Not yet due")
        })
        .unwrap();
        repo.add_action_item(NewActionItem::from_summary(&summary, "No due date"))
            .unwrap();
        let finished = repo
            .add_action_item(NewActionItem {
                due_at: Some(ts(1_699_000_000)),
                ..NewActionItem::from_summary(&summary, "Overdue but done")
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
            .add_action_item(NewActionItem::from_summary(&summary, "Unowned"))
            .unwrap();

        repo.assign_action_item(item.id, Some("jordan")).unwrap();
        assert_eq!(
            repo.action_items(summary.id).unwrap()[0].owner.as_deref(),
            Some("jordan")
        );
    }

    /// Renamed from `..._cascades_through_summary_to_decisions`: since v6 a decision
    /// cascades from its *meeting* directly. Deleting the meeting still removes it, but for
    /// a different reason, and the old name described a path that no longer exists.
    #[test]
    fn deleting_a_meeting_removes_its_decisions() {
        let (db, summary) = setup();
        let repo = SummaryRepository::new(&db);
        repo.add_decision(NewDecision::from_summary(&summary, "Doomed"))
            .unwrap();

        MeetingRepository::new(&db)
            .delete(summary.meeting_id)
            .unwrap();

        assert!(repo.get(summary.id).is_err());
        assert_eq!(
            repo.decisions_for_meeting(summary.meeting_id)
                .unwrap()
                .len(),
            0
        );
    }

    /// The inverse of the migration's regression test, at the repository level: a summary
    /// can be replaced without taking the work it proposed with it.
    #[test]
    fn regenerating_a_summary_keeps_its_action_items() {
        let (db, first) = setup();
        let repo = SummaryRepository::new(&db);

        let item = repo
            .add_action_item(NewActionItem {
                owner: Some("priya".into()),
                ..NewActionItem::from_summary(&first, "Ship the thing")
            })
            .unwrap();
        repo.set_action_item_status(item.id, WorkStatus::InProgress)
            .unwrap();

        repo.delete(first.id).unwrap();

        let survivors = repo.action_items_for_meeting(first.meeting_id).unwrap();
        assert_eq!(
            survivors.len(),
            1,
            "the action item must outlive the summary"
        );
        assert_eq!(survivors[0].owner.as_deref(), Some("priya"));
        assert_eq!(survivors[0].status, WorkStatus::InProgress);
        assert_eq!(
            survivors[0].summary_id, None,
            "provenance degrades to None rather than deleting the row"
        );
    }

    /// A work item whose summary belongs to a different meeting is a silent corruption:
    /// it would file a decision into the wrong meeting's history.
    #[test]
    fn a_summary_from_another_meeting_is_rejected() {
        let (db, summary) = setup();
        let other = MeetingRepository::new(&db)
            .create(NewMeeting {
                project_id: None,
                title: "Somewhere else".into(),
                source: MeetingSource::Import,
                started_at: ts(1_700_000_000),
            })
            .unwrap();

        let err = SummaryRepository::new(&db)
            .add_action_item(NewActionItem {
                meeting_id: other.id,
                summary_id: Some(summary.id),
                text: "Filed against the wrong meeting".into(),
                owner: None,
                owner_person_id: None,
                due_at: None,
            })
            .expect_err("mismatched meeting and summary must be refused");

        assert!(
            matches!(err, StorageError::Invalid { .. }),
            "expected Invalid, got {err:?}"
        );
    }

    #[test]
    fn missing_action_item_status_update_reports_not_found() {
        let (db, _) = setup();
        let err = SummaryRepository::new(&db)
            .set_action_item_status(Id::new(), WorkStatus::Done)
            .expect_err("should be missing");
        assert!(matches!(
            err,
            StorageError::NotFound {
                kind: "ActionItem",
                ..
            }
        ));
    }
}
