use chrono::Utc;
use rusqlite::Row;

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;
use crate::models::{ActionItem, MeetingSeries};

use super::summary::map_action_item;

#[derive(Debug, Clone)]
pub struct NewMeetingSeries {
    pub title: String,
    pub project_id: Option<Id>,
}

#[derive(Debug)]
pub struct MeetingSeriesRepository<'a> {
    db: &'a Database,
}

impl<'a> MeetingSeriesRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn create(&self, new: NewMeetingSeries) -> Result<MeetingSeries> {
        let now = Utc::now();
        let series = MeetingSeries {
            id: Id::new(),
            title: new.title,
            project_id: new.project_id,
            created_at: now,
            updated_at: now,
        };

        self.db.conn().execute(
            "INSERT INTO meeting_series (id, title, project_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                series.id,
                series.title,
                series.project_id,
                series.created_at,
                series.updated_at
            ],
        )?;

        Ok(series)
    }

    pub fn get(&self, id: Id) -> Result<MeetingSeries> {
        self.db
            .conn()
            .query_row(
                "SELECT id, title, project_id, created_at, updated_at
                 FROM meeting_series WHERE id = ?1",
                rusqlite::params![id],
                map_series,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    StorageError::not_found("MeetingSeries", id)
                }
                other => other.into(),
            })
    }

    pub fn list(&self) -> Result<Vec<MeetingSeries>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, title, project_id, created_at, updated_at
             FROM meeting_series ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], map_series)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Find a series by title, creating it if absent.
    ///
    /// Title is the only signal available without a calendar connected — a recurring meeting
    /// recorded by hand looks like "Standup" every week and nothing else distinguishes it.
    /// Matching is case-insensitive and ignores surrounding whitespace. Once calendar import
    /// lands, a recurrence id is a better key and should take precedence over this.
    pub fn find_or_create_by_title(&self, title: &str) -> Result<MeetingSeries> {
        let title = title.trim();
        let existing: Option<MeetingSeries> = self
            .db
            .conn()
            .query_row(
                "SELECT id, title, project_id, created_at, updated_at
                 FROM meeting_series WHERE title = ?1 COLLATE NOCASE",
                rusqlite::params![title],
                map_series,
            )
            .ok();

        match existing {
            Some(series) => Ok(series),
            None => self.create(NewMeetingSeries {
                title: title.to_string(),
                project_id: None,
            }),
        }
    }

    /// Put a meeting into a series, or take it out with `None`.
    pub fn assign_meeting(&self, meeting_id: Id, series_id: Option<Id>) -> Result<()> {
        let changed = self.db.conn().execute(
            "UPDATE meetings SET series_id = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![meeting_id, series_id, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Meeting", meeting_id));
        }
        Ok(())
    }

    /// Meeting ids in a series, most recent first.
    pub fn meeting_ids(&self, series_id: Id) -> Result<Vec<Id>> {
        let conn = self.db.conn();
        let mut stmt =
            conn.prepare("SELECT id FROM meetings WHERE series_id = ?1 ORDER BY started_at DESC")?;
        let rows = stmt.query_map(rusqlite::params![series_id], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The instance of this series immediately before `before`, if there is one.
    ///
    /// Ordered by `started_at` rather than insertion, so importing an old recording after the
    /// fact still lands in the right place in the history.
    pub fn previous_meeting(&self, series_id: Id, before: Id) -> Result<Option<Id>> {
        Ok(self
            .db
            .conn()
            .query_row(
                "SELECT prev.id FROM meetings prev
                   JOIN meetings cur ON cur.id = ?2
                  WHERE prev.series_id = ?1 AND prev.started_at < cur.started_at
                  ORDER BY prev.started_at DESC LIMIT 1",
                rusqlite::params![series_id, before],
                |row| row.get(0),
            )
            .ok())
    }

    /// Action items still open from *earlier* meetings in a series.
    ///
    /// This is the query the whole series concept exists for: what a recurring meeting has
    /// been carrying, rather than what any single instance produced. Excludes the meeting
    /// passed in, so a pre-meeting brief shows inherited work rather than its own.
    pub fn unfinished_business(&self, series_id: Id, before: Id) -> Result<Vec<ActionItem>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT a.id, a.meeting_id, a.summary_id, a.text, a.owner, a.owner_person_id,
                    a.due_at, a.status, a.created_at, a.updated_at
               FROM action_items a
               JOIN meetings m   ON m.id = a.meeting_id
               JOIN meetings cur ON cur.id = ?2
              WHERE m.series_id = ?1
                AND m.started_at < cur.started_at
                AND a.status IN ('todo', 'in_progress')
              ORDER BY a.due_at IS NULL, a.due_at, m.started_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![series_id, before], map_action_item)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }
}

fn map_series(row: &Row<'_>) -> rusqlite::Result<MeetingSeries> {
    Ok(MeetingSeries {
        id: row.get(0)?,
        title: row.get(1)?,
        project_id: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MeetingSource, WorkStatus};
    use crate::repositories::{MeetingRepository, NewActionItem, NewMeeting, SummaryRepository};
    use chrono::{DateTime, TimeZone};

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0)
            .single()
            .expect("valid timestamp")
    }

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn meeting(db: &Database, title: &str, started: i64) -> Id {
        MeetingRepository::new(db)
            .create(NewMeeting {
                project_id: None,
                title: title.into(),
                source: MeetingSource::Import,
                started_at: ts(started),
            })
            .unwrap()
            .id
    }

    #[test]
    fn find_or_create_by_title_threads_repeats_without_duplicating() {
        let db = db();
        let repo = MeetingSeriesRepository::new(&db);

        let first = repo.find_or_create_by_title("Weekly Standup").unwrap();
        let again = repo.find_or_create_by_title("  weekly standup  ").unwrap();

        assert_eq!(first.id, again.id);
        assert_eq!(repo.list().unwrap().len(), 1);
    }

    #[test]
    fn meetings_in_a_series_come_back_newest_first() {
        let db = db();
        let repo = MeetingSeriesRepository::new(&db);
        let series = repo.find_or_create_by_title("Standup").unwrap();

        let older = meeting(&db, "Standup", 1_700_000_000);
        let newer = meeting(&db, "Standup", 1_700_600_000);
        repo.assign_meeting(older, Some(series.id)).unwrap();
        repo.assign_meeting(newer, Some(series.id)).unwrap();

        assert_eq!(repo.meeting_ids(series.id).unwrap(), vec![newer, older]);
    }

    #[test]
    fn previous_meeting_is_ordered_by_start_time_not_insertion() {
        let db = db();
        let repo = MeetingSeriesRepository::new(&db);
        let series = repo.find_or_create_by_title("Standup").unwrap();

        let newest = meeting(&db, "Standup", 1_700_600_000);
        // Imported after the fact, but happened first.
        let oldest = meeting(&db, "Standup", 1_700_000_000);
        let middle = meeting(&db, "Standup", 1_700_300_000);
        for m in [newest, oldest, middle] {
            repo.assign_meeting(m, Some(series.id)).unwrap();
        }

        assert_eq!(
            repo.previous_meeting(series.id, newest).unwrap(),
            Some(middle)
        );
        assert_eq!(repo.previous_meeting(series.id, oldest).unwrap(), None);
    }

    #[test]
    fn unfinished_business_carries_open_items_forward_and_leaves_closed_ones() {
        let db = db();
        let series_repo = MeetingSeriesRepository::new(&db);
        let summaries = SummaryRepository::new(&db);
        let series = series_repo.find_or_create_by_title("Standup").unwrap();

        let last_week = meeting(&db, "Standup", 1_700_000_000);
        let this_week = meeting(&db, "Standup", 1_700_600_000);
        series_repo
            .assign_meeting(last_week, Some(series.id))
            .unwrap();
        series_repo
            .assign_meeting(this_week, Some(series.id))
            .unwrap();

        let still_open = summaries
            .add_action_item(NewActionItem::on_meeting(last_week, "Chase the vendor"))
            .unwrap();
        let finished = summaries
            .add_action_item(NewActionItem::on_meeting(last_week, "Book the room"))
            .unwrap();
        summaries
            .set_action_item_status(finished.id, WorkStatus::Done)
            .unwrap();
        // Belongs to this week, so it is not *inherited* business.
        summaries
            .add_action_item(NewActionItem::on_meeting(this_week, "Raised just now"))
            .unwrap();

        let carried = series_repo
            .unfinished_business(series.id, this_week)
            .unwrap();

        assert_eq!(carried.len(), 1, "got {carried:?}");
        assert_eq!(carried[0].id, still_open.id);
    }

    #[test]
    fn the_first_meeting_in_a_series_inherits_nothing() {
        let db = db();
        let repo = MeetingSeriesRepository::new(&db);
        let series = repo.find_or_create_by_title("Standup").unwrap();
        let first = meeting(&db, "Standup", 1_700_000_000);
        repo.assign_meeting(first, Some(series.id)).unwrap();

        SummaryRepository::new(&db)
            .add_action_item(NewActionItem::on_meeting(first, "Its own work"))
            .unwrap();

        assert!(repo
            .unfinished_business(series.id, first)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_meeting_can_be_taken_out_of_its_series() {
        let db = db();
        let repo = MeetingSeriesRepository::new(&db);
        let series = repo.find_or_create_by_title("Standup").unwrap();
        let m = meeting(&db, "Standup", 1_700_000_000);

        repo.assign_meeting(m, Some(series.id)).unwrap();
        repo.assign_meeting(m, None).unwrap();

        assert!(repo.meeting_ids(series.id).unwrap().is_empty());
    }

    #[test]
    fn assigning_a_missing_meeting_reports_not_found() {
        let db = db();
        let repo = MeetingSeriesRepository::new(&db);
        let series = repo.find_or_create_by_title("Standup").unwrap();

        let err = repo
            .assign_meeting(Id::new(), Some(series.id))
            .expect_err("should be missing");
        assert!(matches!(err, StorageError::NotFound { .. }), "got {err:?}");
    }
}
