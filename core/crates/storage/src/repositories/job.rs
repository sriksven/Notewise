//! Scheduled jobs and the record of what they did.
//!
//! # Why runs are persisted when agent runs are not
//!
//! `agent.rs` keeps its runs in memory and argues a trace is only interesting while it is happening
//! or just after. That argument assumes somebody was present. For a run at 6am nobody was, and "it
//! failed on Tuesday and I need to know why" is the ordinary case rather than an edge one.
//!
//! Retention is bounded on write. A job firing every fifteen minutes would otherwise grow this table
//! forever — a disk-space bug with a slow fuse.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row};

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;

/// How many runs are kept per job.
///
/// Enough to see a pattern — a job that fails every Monday — without keeping a year of traces for
/// something that runs every quarter hour.
pub const RUNS_KEPT: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: Id,
    pub name: String,
    pub prompt: String,
    pub cron: String,
    pub timezone: String,
    pub enabled: bool,
    pub catch_up: bool,
    pub timeout_secs: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewJob {
    pub name: String,
    pub prompt: String,
    pub cron: String,
    pub timezone: String,
    pub catch_up: bool,
    pub timeout_secs: i64,
}

/// How a run ended, or that it has not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    TimedOut,
    /// The occurrence was not served: a previous run was still going, or occurrences were missed.
    Skipped,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::TimedOut => "timed_out",
            RunStatus::Skipped => "skipped",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => RunStatus::Running,
            "completed" => RunStatus::Completed,
            "failed" => RunStatus::Failed,
            "timed_out" => RunStatus::TimedOut,
            "skipped" => RunStatus::Skipped,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRun {
    pub id: Id,
    pub job_id: Id,
    pub status: RunStatus,
    /// JSON array of steps, as the agent produced them.
    pub trace: Option<String>,
    pub note_id: Option<Id>,
    /// External tool calls this run proposed. Always zero until the MCP client exists.
    pub proposals: i64,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub struct JobRepository<'a> {
    db: &'a Database,
}

impl<'a> JobRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn create(&self, new: NewJob) -> Result<Job> {
        let now = Utc::now();
        let job = Job {
            id: Id::new(),
            name: new.name,
            prompt: new.prompt,
            cron: new.cron,
            timezone: new.timezone,
            enabled: true,
            catch_up: new.catch_up,
            timeout_secs: new.timeout_secs,
            created_at: now,
            updated_at: now,
        };

        self.db
            .conn()
            .execute(
                "INSERT INTO jobs
                    (id, name, prompt, cron, timezone, enabled, catch_up, timeout_secs,
                     created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                rusqlite::params![
                    job.id,
                    job.name,
                    job.prompt,
                    job.cron,
                    job.timezone,
                    job.enabled,
                    job.catch_up,
                    job.timeout_secs,
                    now,
                ],
            )
            .map_err(|e| match e {
                // The name is unique so a job can be referred to unambiguously in a notification.
                rusqlite::Error::SqliteFailure(f, _)
                    if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StorageError::Refused(format!("a job named '{}' already exists", job.name))
                }
                other => other.into(),
            })?;

        Ok(job)
    }

    pub fn list(&self) -> Result<Vec<Job>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, prompt, cron, timezone, enabled, catch_up, timeout_secs,
                    created_at, updated_at
               FROM jobs ORDER BY name",
        )?;
        let rows = stmt.query_map([], map_job)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get(&self, id: Id) -> Result<Job> {
        let conn = self.db.conn();
        conn.query_row(
            "SELECT id, name, prompt, cron, timezone, enabled, catch_up, timeout_secs,
                    created_at, updated_at
               FROM jobs WHERE id = ?1",
            rusqlite::params![id],
            map_job,
        )
        .optional()?
        .ok_or_else(|| StorageError::not_found("Job", id))
    }

    pub fn set_enabled(&self, id: Id, enabled: bool) -> Result<Job> {
        let changed = self.db.conn().execute(
            "UPDATE jobs SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, enabled, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("Job", id));
        }
        self.get(id)
    }

    pub fn delete(&self, id: Id) -> Result<()> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM jobs WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::not_found("Job", id));
        }
        Ok(())
    }

    /// Start a run, returning its id.
    pub fn start_run(&self, job_id: Id, at: DateTime<Utc>) -> Result<Id> {
        let id = Id::new();
        self.db.conn().execute(
            "INSERT INTO job_runs (id, job_id, status, started_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, job_id, RunStatus::Running.as_str(), at],
        )?;
        self.prune(job_id)?;
        Ok(id)
    }

    /// Record a run that was never attempted, and why.
    ///
    /// A skip is a row rather than silence: a job whose schedule is too tight, or an app that was
    /// closed over a weekend, shows up as a visible gap the user can act on.
    pub fn record_skip(&self, job_id: Id, at: DateTime<Utc>, reason: &str) -> Result<Id> {
        let id = Id::new();
        self.db.conn().execute(
            "INSERT INTO job_runs (id, job_id, status, error, started_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            rusqlite::params![id, job_id, RunStatus::Skipped.as_str(), reason, at],
        )?;
        self.prune(job_id)?;
        Ok(id)
    }

    /// Finish a run.
    pub fn finish_run(
        &self,
        run_id: Id,
        status: RunStatus,
        trace: Option<&str>,
        note_id: Option<Id>,
        error: Option<&str>,
    ) -> Result<()> {
        let changed = self.db.conn().execute(
            "UPDATE job_runs
                SET status = ?2, trace = ?3, note_id = ?4, error = ?5, finished_at = ?6
              WHERE id = ?1",
            rusqlite::params![run_id, status.as_str(), trace, note_id, error, Utc::now()],
        )?;
        if changed == 0 {
            return Err(StorageError::not_found("JobRun", run_id));
        }
        Ok(())
    }

    pub fn runs(&self, job_id: Id, limit: usize) -> Result<Vec<JobRun>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, job_id, status, trace, note_id, proposals, error, started_at, finished_at
               FROM job_runs WHERE job_id = ?1 ORDER BY started_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![job_id, limit as i64], map_run)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// When this job last started a run, and whether one is in flight.
    ///
    /// Both in one query because the scheduler needs both for every job on every tick, and two
    /// round trips per job per tick is the kind of thing that stops being free at fifty jobs.
    pub fn timing(&self, job_id: Id) -> Result<(Option<DateTime<Utc>>, bool)> {
        let conn = self.db.conn();
        let last: Option<DateTime<Utc>> = conn
            .query_row(
                "SELECT MAX(started_at) FROM job_runs
                  WHERE job_id = ?1 AND status <> 'skipped'",
                rusqlite::params![job_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();

        let running: i64 = conn.query_row(
            "SELECT COUNT(*) FROM job_runs WHERE job_id = ?1 AND status = 'running'",
            rusqlite::params![job_id],
            |r| r.get(0),
        )?;

        Ok((last, running > 0))
    }

    /// Keep only the most recent [`RUNS_KEPT`] runs for a job.
    fn prune(&self, job_id: Id) -> Result<()> {
        self.db.conn().execute(
            "DELETE FROM job_runs
              WHERE job_id = ?1
                AND id NOT IN (
                    SELECT id FROM job_runs WHERE job_id = ?1
                     ORDER BY started_at DESC LIMIT ?2
                )",
            rusqlite::params![job_id, RUNS_KEPT as i64],
        )?;
        Ok(())
    }
}

fn map_job(row: &Row<'_>) -> rusqlite::Result<Job> {
    Ok(Job {
        id: row.get(0)?,
        name: row.get(1)?,
        prompt: row.get(2)?,
        cron: row.get(3)?,
        timezone: row.get(4)?,
        enabled: row.get(5)?,
        catch_up: row.get(6)?,
        timeout_secs: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn map_run(row: &Row<'_>) -> rusqlite::Result<JobRun> {
    let raw: String = row.get(2)?;
    Ok(JobRun {
        id: row.get(0)?,
        job_id: row.get(1)?,
        // A status this build does not know is reported, not guessed at — the same treatment every
        // other stored enum here gets.
        status: RunStatus::parse(&raw).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                format!("unknown run status '{raw}'").into(),
            )
        })?,
        trace: row.get(3)?,
        note_id: row.get(4)?,
        proposals: row.get(5)?,
        error: row.get(6)?,
        started_at: row.get(7)?,
        finished_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn new_job(name: &str) -> NewJob {
        NewJob {
            name: name.into(),
            prompt: "summarise this week's decisions".into(),
            cron: "0 9 * * MON".into(),
            timezone: "UTC".into(),
            catch_up: false,
            timeout_secs: 600,
        }
    }

    #[test]
    fn a_job_round_trips() {
        let db = db();
        let repo = JobRepository::new(&db);
        let made = repo.create(new_job("Weekly digest")).expect("create");

        assert_eq!(repo.get(made.id).expect("get"), made);
        assert!(made.enabled, "a new job is on; creating one is the intent");
        assert_eq!(repo.list().expect("list").len(), 1);
    }

    #[test]
    fn two_jobs_cannot_share_a_name() {
        let db = db();
        let repo = JobRepository::new(&db);
        repo.create(new_job("Weekly digest")).expect("first");

        let err = repo.create(new_job("Weekly digest")).expect_err("second");
        assert!(matches!(err, StorageError::Refused(_)), "{err:?}");
    }

    #[test]
    fn a_job_can_be_paused_without_losing_its_history() {
        let db = db();
        let repo = JobRepository::new(&db);
        let job = repo.create(new_job("Weekly digest")).expect("create");
        repo.start_run(job.id, Utc::now()).expect("run");

        let paused = repo.set_enabled(job.id, false).expect("disable");
        assert!(!paused.enabled);
        assert_eq!(repo.runs(job.id, 10).expect("runs").len(), 1);
    }

    #[test]
    fn a_run_records_what_it_produced() {
        let db = db();
        let repo = JobRepository::new(&db);
        let job = repo.create(new_job("Weekly digest")).expect("create");

        let run = repo.start_run(job.id, Utc::now()).expect("start");
        repo.finish_run(
            run,
            RunStatus::Completed,
            Some(r#"[{"n":1,"action":"search"}]"#),
            None,
            None,
        )
        .expect("finish");

        let runs = repo.runs(job.id, 10).expect("runs");
        assert_eq!(runs[0].status, RunStatus::Completed);
        assert!(runs[0].trace.as_deref().unwrap().contains("search"));
        assert!(runs[0].finished_at.is_some());
    }

    /// A skip is a row rather than silence, so a schedule that is too tight is visible.
    #[test]
    fn a_skip_is_recorded_with_its_reason() {
        let db = db();
        let repo = JobRepository::new(&db);
        let job = repo.create(new_job("Weekly digest")).expect("create");

        repo.record_skip(job.id, Utc::now(), "previous run still going")
            .expect("skip");

        let runs = repo.runs(job.id, 10).expect("runs");
        assert_eq!(runs[0].status, RunStatus::Skipped);
        assert_eq!(runs[0].error.as_deref(), Some("previous run still going"));
    }

    /// A skip must not look like a run for scheduling purposes, or one skip would push the next
    /// occurrence a whole interval away and the job would quietly halve its frequency.
    #[test]
    fn a_skip_does_not_count_as_having_run() {
        let db = db();
        let repo = JobRepository::new(&db);
        let job = repo.create(new_job("Weekly digest")).expect("create");

        repo.record_skip(job.id, Utc::now(), "busy").expect("skip");
        let (last, running) = repo.timing(job.id).expect("timing");

        assert_eq!(last, None, "a skip is not a run");
        assert!(!running);
    }

    #[test]
    fn timing_reports_a_run_in_flight() {
        let db = db();
        let repo = JobRepository::new(&db);
        let job = repo.create(new_job("Weekly digest")).expect("create");
        let run = repo.start_run(job.id, Utc::now()).expect("start");

        let (last, running) = repo.timing(job.id).expect("timing");
        assert!(last.is_some());
        assert!(running);

        repo.finish_run(run, RunStatus::Completed, None, None, None)
            .expect("finish");
        assert!(!repo.timing(job.id).expect("timing").1);
    }

    #[test]
    fn run_history_is_bounded() {
        let db = db();
        let repo = JobRepository::new(&db);
        let job = repo.create(new_job("Frequent")).expect("create");

        let base = Utc::now();
        for n in 0..(RUNS_KEPT as i64 + 20) {
            repo.start_run(job.id, base + Duration::seconds(n))
                .expect("start");
        }

        let runs = repo.runs(job.id, 1000).expect("runs");
        assert_eq!(
            runs.len(),
            RUNS_KEPT,
            "an unbounded trace table on a job firing every fifteen minutes is a slow-fuse bug"
        );
        // The ones kept are the newest.
        assert!(runs[0].started_at > runs[runs.len() - 1].started_at);
    }

    #[test]
    fn deleting_a_job_takes_its_runs_with_it() {
        let db = db();
        let repo = JobRepository::new(&db);
        let job = repo.create(new_job("Weekly digest")).expect("create");
        repo.start_run(job.id, Utc::now()).expect("start");

        repo.delete(job.id).expect("delete");
        assert!(repo.get(job.id).is_err());
        assert!(repo.runs(job.id, 10).expect("runs").is_empty());
    }

    #[test]
    fn a_missing_job_is_reported_not_panicked() {
        let db = db();
        let repo = JobRepository::new(&db);
        let ghost = Id::new();

        assert!(repo.get(ghost).is_err());
        assert!(repo.delete(ghost).is_err());
        assert!(repo.set_enabled(ghost, false).is_err());
    }

    #[test]
    fn run_statuses_round_trip() {
        for status in [
            RunStatus::Running,
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::TimedOut,
            RunStatus::Skipped,
        ] {
            assert_eq!(RunStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(RunStatus::parse("elsewhere"), None);
    }
}
