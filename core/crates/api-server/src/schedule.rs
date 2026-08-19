//! Deciding when a scheduled job is due.
//!
//! Pure, with the clock passed in. Every rule here — a job whose previous run has not finished, an
//! app that was closed over a weekend, a cron expression that fires every minute — is answerable
//! without waiting for wall time, which is the only way this is testable at all.
//!
//! The running of the work lives in [`crate::jobs`]. This module only ever answers "should it run,
//! and if not, why not".

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use std::str::FromStr;

/// Why a job is or is not being run now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Due {
    /// Run it. Carries the occurrence being served, which is what gets recorded.
    Run { occurrence: DateTime<Utc> },
    /// Not yet.
    NotDue,
    /// The previous run has not finished.
    ///
    /// Skipped rather than queued: a job that takes longer than its interval would otherwise
    /// accumulate a backlog that never drains, and every run in it costs model calls. Skipping is
    /// self-correcting, and the record of it is where a user sees their schedule is too tight.
    AlreadyRunning,
    /// Occurrences were missed while the app was closed, and `catch_up` is off.
    ///
    /// Carries how many, so the run history can say so rather than showing an unexplained gap.
    Missed { count: usize },
    /// The expression does not parse. Recorded rather than retried.
    Invalid { reason: String },
}

/// What the scheduler needs to know about a job to decide.
#[derive(Debug, Clone)]
pub struct JobTiming<'a> {
    pub cron: &'a str,
    /// IANA name, e.g. `Europe/London`. "Every Friday at 5pm" means the user's Friday.
    pub timezone: &'a str,
    pub enabled: bool,
    /// Run the single most recent missed occurrence after a gap.
    pub catch_up: bool,
    /// When this job last started a run, if ever.
    pub last_started: Option<DateTime<Utc>>,
    /// Whether a run is in flight.
    pub running: bool,
}

/// Parse a cron expression, reporting why it failed.
///
/// Exposed so a job can be validated at save time. An expression discovered to be invalid at 3am is
/// a job that silently never ran.
pub fn parse_cron(expr: &str) -> Result<Schedule, String> {
    // The `cron` crate wants seconds as a sixth leading field. Users write five-field cron, so a
    // leading `0` is added when five fields are given — otherwise "0 9 * * *" would be read as
    // second-0 minute-9 and fire once a minute for an hour, which is a spectacular way to be wrong.
    let trimmed = expr.trim();
    let fields = trimmed.split_whitespace().count();
    let normalized = match fields {
        5 => format!("0 {trimmed}"),
        6 => trimmed.to_string(),
        n => return Err(format!("a cron expression needs 5 fields, got {n}")),
    };

    Schedule::from_str(&normalized).map_err(|e| e.to_string())
}

/// The next few times a job would fire, for showing before it is saved.
pub fn next_fires(
    expr: &str,
    timezone: &str,
    after: DateTime<Utc>,
    count: usize,
) -> Result<Vec<DateTime<Utc>>, String> {
    let schedule = parse_cron(expr)?;
    let tz = timezone_of(timezone)?;

    Ok(schedule
        .after(&after.with_timezone(&tz))
        .take(count)
        .map(|t| t.with_timezone(&Utc))
        .collect())
}

fn timezone_of(name: &str) -> Result<Tz, String> {
    name.parse::<Tz>()
        .map_err(|_| format!("'{name}' is not an IANA timezone name"))
}

/// Whether `job` should run at `now`.
pub fn due(job: &JobTiming<'_>, now: DateTime<Utc>) -> Due {
    if !job.enabled {
        return Due::NotDue;
    }

    let schedule = match parse_cron(job.cron) {
        Ok(s) => s,
        Err(reason) => return Due::Invalid { reason },
    };
    let tz = match timezone_of(job.timezone) {
        Ok(tz) => tz,
        Err(reason) => return Due::Invalid { reason },
    };

    // Never run: the first occurrence is in the future, so there is nothing to catch up on and
    // nothing owed. A job created at noon does not immediately fire this morning's schedule.
    let Some(since) = job.last_started else {
        return Due::NotDue;
    };

    // Occurrences strictly after the last run and at or before now.
    let occurrences: Vec<DateTime<Utc>> = schedule
        .after(&since.with_timezone(&tz))
        .take_while(|t| t.with_timezone(&Utc) <= now)
        // Bounded so a job that fires every minute and has not run for a year cannot make this
        // loop walk half a million occurrences before answering.
        .take(1_000)
        .map(|t| t.with_timezone(&Utc))
        .collect();

    let Some(latest) = occurrences.last().copied() else {
        return Due::NotDue;
    };

    // Checked after establishing that something is owed, so a job that is merely running while
    // nothing is due reports `NotDue` rather than a misleading skip.
    if job.running {
        return Due::AlreadyRunning;
    }

    // More than one owed means the app was not running for at least one whole interval.
    if occurrences.len() > 1 && !job.catch_up {
        return Due::Missed {
            count: occurrences.len(),
        };
    }

    // With `catch_up`, the *most recent* missed occurrence runs — once. Never the whole backlog:
    // opening the app after a holiday and receiving a week of stale digests, each costing model
    // time and describing days the user already lived through, is the worst first impression this
    // feature could make.
    Due::Run { occurrence: latest }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("timestamp")
            .with_timezone(&Utc)
    }

    fn job<'a>(cron: &'a str, last: Option<DateTime<Utc>>) -> JobTiming<'a> {
        JobTiming {
            cron,
            timezone: "UTC",
            enabled: true,
            catch_up: false,
            last_started: last,
            running: false,
        }
    }

    /// Five-field cron is what people write, and reading it as six would fire sixty times an hour.
    #[test]
    fn five_field_cron_is_read_as_five_fields() {
        let next = next_fires("0 9 * * *", "UTC", utc("2026-08-19T00:00:00Z"), 3).expect("parses");
        assert_eq!(
            next,
            vec![
                utc("2026-08-19T09:00:00Z"),
                utc("2026-08-20T09:00:00Z"),
                utc("2026-08-21T09:00:00Z"),
            ],
            "daily at 09:00, not once a minute"
        );
    }

    #[test]
    fn six_field_cron_still_works() {
        assert!(next_fires("30 0 9 * * *", "UTC", utc("2026-08-19T00:00:00Z"), 1).is_ok());
    }

    #[test]
    fn a_bad_expression_is_refused_with_a_reason() {
        for bad in ["", "not cron", "* * *", "* * * * * * *", "99 * * * *"] {
            assert!(parse_cron(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn an_unknown_timezone_is_refused() {
        let err = next_fires("0 9 * * *", "Mars/Olympus", utc("2026-08-19T00:00:00Z"), 1)
            .expect_err("must refuse");
        assert!(err.contains("IANA"), "{err}");
    }

    /// "Every Friday at 5pm" means the user's Friday, so the same expression is a different instant
    /// in a different zone.
    #[test]
    fn the_timezone_moves_the_fire_time() {
        let london = next_fires(
            "0 17 * * FRI",
            "Europe/London",
            utc("2026-08-19T00:00:00Z"),
            1,
        )
        .unwrap();
        let tokyo =
            next_fires("0 17 * * FRI", "Asia/Tokyo", utc("2026-08-19T00:00:00Z"), 1).unwrap();
        assert_ne!(london, tokyo);
    }

    #[test]
    fn a_job_that_has_never_run_is_not_immediately_due() {
        assert_eq!(
            due(&job("0 9 * * *", None), utc("2026-08-19T12:00:00Z")),
            Due::NotDue,
            "creating a job at noon must not fire this morning's occurrence"
        );
    }

    #[test]
    fn a_job_is_due_once_its_occurrence_has_passed() {
        let last = utc("2026-08-19T09:00:00Z");
        assert_eq!(
            due(&job("0 9 * * *", Some(last)), utc("2026-08-19T23:00:00Z")),
            Due::NotDue,
            "the next occurrence is tomorrow"
        );
        assert_eq!(
            due(&job("0 9 * * *", Some(last)), utc("2026-08-20T09:00:00Z")),
            Due::Run {
                occurrence: utc("2026-08-20T09:00:00Z")
            }
        );
    }

    #[test]
    fn a_disabled_job_never_runs() {
        let mut j = job("* * * * *", Some(utc("2026-08-19T00:00:00Z")));
        j.enabled = false;
        assert_eq!(due(&j, utc("2026-08-19T12:00:00Z")), Due::NotDue);
    }

    /// Skipped, not queued. A backlog of model calls never drains.
    #[test]
    fn a_job_still_running_is_skipped() {
        let mut j = job("0 9 * * *", Some(utc("2026-08-19T09:00:00Z")));
        j.running = true;
        assert_eq!(due(&j, utc("2026-08-20T09:00:00Z")), Due::AlreadyRunning);
    }

    #[test]
    fn a_running_job_with_nothing_owed_is_merely_not_due() {
        let mut j = job("0 9 * * *", Some(utc("2026-08-19T09:00:00Z")));
        j.running = true;
        assert_eq!(
            due(&j, utc("2026-08-19T10:00:00Z")),
            Due::NotDue,
            "reporting a skip when nothing was owed would be a misleading run record"
        );
    }

    /// Closed Friday to Monday: three occurrences owed, none of them fired.
    #[test]
    fn missed_occurrences_are_reported_not_backfilled() {
        let j = job("0 9 * * *", Some(utc("2026-08-14T09:00:00Z")));
        assert_eq!(
            due(&j, utc("2026-08-17T09:00:00Z")),
            Due::Missed { count: 3 },
            "a burst of stale digests describing days the user already lived through"
        );
    }

    #[test]
    fn catch_up_runs_only_the_most_recent_missed_occurrence() {
        let mut j = job("0 9 * * *", Some(utc("2026-08-14T09:00:00Z")));
        j.catch_up = true;
        assert_eq!(
            due(&j, utc("2026-08-17T09:00:00Z")),
            Due::Run {
                occurrence: utc("2026-08-17T09:00:00Z")
            },
            "the latest, once — never the whole backlog"
        );
    }

    #[test]
    fn an_invalid_expression_is_reported_rather_than_retried_forever() {
        let j = job("nonsense", Some(utc("2026-08-19T00:00:00Z")));
        assert!(matches!(
            due(&j, utc("2026-08-19T12:00:00Z")),
            Due::Invalid { .. }
        ));
    }

    /// A minutely job untouched for a year must not make the decision walk half a million
    /// occurrences before answering.
    #[test]
    fn a_long_gap_on_a_frequent_schedule_still_answers_quickly() {
        let j = job("* * * * *", Some(utc("2025-08-19T00:00:00Z")));
        let started = std::time::Instant::now();
        let outcome = due(&j, utc("2026-08-19T00:00:00Z"));
        assert!(matches!(outcome, Due::Missed { .. }));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "took {:?}",
            started.elapsed()
        );
    }
}
