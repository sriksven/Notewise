//! Turning imported calendar events into things the workspace understands.
//!
//! The importer lands raw events. This decides what they *mean*: which meeting an event belongs to,
//! which person an attendee is, and which series a recurring event forms.
//!
//! # Why this is here and not in a crate of its own
//!
//! The design said a separate `notewise-calendar` crate, on the grounds that `connectors` is
//! plumbing and this is domain logic. The first half of that is right and the conclusion is not:
//! `api-server` already depends on `storage` and `graph` and already hosts exactly this kind of
//! module — `indexing`, `retrieval`, `speakers`, `agent`. A new crate would need both dependencies
//! handed to it for no isolation benefit, which is the argument the scheduled-jobs design already
//! makes for keeping its scheduler here.
//!
//! # Why an ambiguous match is left alone
//!
//! A wrong link is worse than no link. It silently attributes decisions and action items to the
//! wrong meeting, and the user has no reason to go looking. So when two meetings both plausibly
//! match one event, neither is linked and the event stays in the queue.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use notewise_graph::{EdgeKind, Graph, NodeKind, NodeRef};
use notewise_storage::{
    CalendarRepository, Database, MeetingRepository, MeetingSeriesRepository, NewPerson,
    PersonRepository,
};

use crate::error::ApiResult;
use crate::state::AppState;

/// The least a meeting must overlap an event to be considered the same occasion.
///
/// Recordings start late and run long, so exact bounds match almost nothing. The threshold is the
/// greater of this fraction of the event and [`MIN_OVERLAP`].
const OVERLAP_FRACTION: f64 = 0.5;

/// The floor, for events short enough that a fraction of them is meaningless.
const MIN_OVERLAP: Duration = Duration::minutes(5);

/// What one reconciliation pass changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub meetings_linked: usize,
    /// Events matching more than one meeting. Left unlinked on purpose.
    pub ambiguous: usize,
    pub people_matched: usize,
    pub people_created: usize,
    pub series_linked: usize,
}

impl ReconcileReport {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// How much two spans overlap.
///
/// Pure, so every boundary is testable without a database.
pub fn overlap(a: (DateTime<Utc>, DateTime<Utc>), b: (DateTime<Utc>, DateTime<Utc>)) -> Duration {
    let start = a.0.max(b.0);
    let end = a.1.min(b.1);
    if end > start {
        end - start
    } else {
        Duration::zero()
    }
}

/// Whether a meeting's span is enough of an event's span to be the same occasion.
pub fn is_same_occasion(
    event: (DateTime<Utc>, DateTime<Utc>),
    meeting: (DateTime<Utc>, DateTime<Utc>),
) -> bool {
    let shared = overlap(event, meeting);
    if shared <= Duration::zero() {
        return false;
    }

    let event_len = event.1 - event.0;
    let fraction = Duration::milliseconds(
        (event_len.num_milliseconds() as f64 * OVERLAP_FRACTION).round() as i64,
    );
    let required = fraction.max(MIN_OVERLAP).min(event_len);

    shared >= required
}

/// Link events to meetings, attendees to people, and recurring events to series.
///
/// Idempotent: a second run over the same data changes nothing.
pub async fn reconcile(state: &Arc<AppState>) -> ApiResult<ReconcileReport> {
    let db = state.db().await;
    let mut report = ReconcileReport::default();

    link_meetings(&db, &mut report)?;
    resolve_attendees(&db, &mut report)?;
    link_series(&db, &mut report)?;

    Ok(report)
}

fn link_meetings(db: &Database, report: &mut ReconcileReport) -> ApiResult<()> {
    let calendar = CalendarRepository::new(db);
    let meetings = MeetingRepository::new(db);
    let graph = Graph::new(db);

    for event in calendar.unlinked(200)? {
        // All-day events are not occasions. Linking a recording to "Company holiday" because it
        // overlaps twenty-four hours would be worse than leaving it alone.
        if event.is_all_day {
            continue;
        }

        let candidates: Vec<_> = meetings
            .overlapping(event.starts_at, event.ends_at)?
            .into_iter()
            .filter(|m| {
                let ended = m.ended_at.unwrap_or_else(Utc::now);
                is_same_occasion((event.starts_at, event.ends_at), (m.started_at, ended))
            })
            .collect();

        match candidates.len() {
            0 => {}
            1 => {
                graph.connect(
                    NodeRef::new(NodeKind::Meeting, candidates[0].id),
                    EdgeKind::SyncedTo,
                    NodeRef::new(NodeKind::ExternalItem, event.external_item_id),
                )?;
                report.meetings_linked += 1;
            }
            // Back-to-back meetings in one afternoon are exactly when this happens, and guessing
            // would attribute one meeting's decisions to another.
            _ => report.ambiguous += 1,
        }
    }

    Ok(())
}

fn resolve_attendees(db: &Database, report: &mut ReconcileReport) -> ApiResult<()> {
    let calendar = CalendarRepository::new(db);
    let people = PersonRepository::new(db);

    for attendee in calendar.unresolved_attendees(500)? {
        let email = attendee.email.trim().to_lowercase();
        if email.is_empty() {
            continue;
        }

        // Matched on email only. Two people called "Sam" are not evidence of anything, and merging
        // them would attribute one person's words to the other.
        let person = match people.find_by_email(&email)? {
            Some(person) => {
                report.people_matched += 1;
                person
            }
            None => {
                let display_name = attendee
                    .display_name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    // Falling back to the local part rather than the whole address: a name is what
                    // a transcript will show, and "priya" reads better than "priya@example.com".
                    .unwrap_or_else(|| email.split('@').next().unwrap_or(&email).to_string());

                report.people_created += 1;
                people.create(NewPerson {
                    display_name,
                    email: Some(email),
                })?
            }
        };

        calendar.set_attendee_person(attendee.id, person.id)?;
    }

    Ok(())
}

fn link_series(db: &Database, report: &mut ReconcileReport) -> ApiResult<()> {
    let calendar = CalendarRepository::new(db);
    let series = MeetingSeriesRepository::new(db);
    let graph = Graph::new(db);

    // Every linked event that belongs to a recurrence, grouped by its key. This is what replaces
    // matching a series by title — two standups three months apart are the same series only if the
    // provider says so.
    for event in calendar.between(
        Utc::now() - Duration::days(365),
        Utc::now() + Duration::days(365),
    )? {
        let Some(key) = event.recurrence_key.as_deref() else {
            continue;
        };

        let linked =
            graph.connections(NodeRef::new(NodeKind::ExternalItem, event.external_item_id))?;
        let Some(meeting) = linked
            .into_iter()
            .filter(|c| c.kind == EdgeKind::SyncedTo)
            .map(|c| c.node)
            .find(|n| n.kind == NodeKind::Meeting)
        else {
            // Not linked to a meeting yet. It will be picked up on a later pass, once a recording
            // exists to link it to.
            continue;
        };

        // The key names the series. Using the provider's identifier rather than the title means a
        // renamed meeting stays in its series and two unrelated "Standup"s do not merge.
        let found = series.find_or_create_by_title(key)?;
        series.assign_meeting(meeting.id, Some(found.id))?;
        report.series_linked += 1;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(mins: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + mins * 60, 0).expect("timestamp")
    }

    #[test]
    fn no_overlap_is_never_the_same_occasion() {
        assert!(!is_same_occasion((at(0), at(30)), (at(31), at(60))));
        // Touching at the boundary is not overlapping.
        assert!(!is_same_occasion((at(0), at(30)), (at(30), at(60))));
    }

    /// A recording that started late and ran long is the ordinary case, not an edge one.
    #[test]
    fn a_late_start_and_long_run_still_matches() {
        // 30-minute event, recording from +5 to +40: 25 of 30 minutes shared.
        assert!(is_same_occasion((at(0), at(30)), (at(5), at(40))));
    }

    #[test]
    fn a_brief_clip_of_a_long_event_does_not_match() {
        // A 60-minute event needs 30 minutes; five is not enough.
        assert!(!is_same_occasion((at(0), at(60)), (at(0), at(5))));
    }

    /// For a short event, half of it is less than the floor, so the floor is what binds.
    #[test]
    fn the_floor_binds_for_short_events() {
        // A 6-minute event: half is 3 minutes, but the floor is 5.
        assert!(!is_same_occasion((at(0), at(6)), (at(0), at(4))));
        assert!(is_same_occasion((at(0), at(6)), (at(0), at(6))));
    }

    /// An event shorter than the floor can still be matched, or a two-minute event could never be.
    #[test]
    fn an_event_shorter_than_the_floor_can_still_match() {
        assert!(is_same_occasion((at(0), at(2)), (at(0), at(2))));
        assert!(!is_same_occasion((at(0), at(2)), (at(1), at(1))));
    }

    #[test]
    fn overlap_is_symmetric_and_never_negative() {
        assert_eq!(
            overlap((at(0), at(30)), (at(10), at(20))),
            Duration::minutes(10)
        );
        assert_eq!(
            overlap((at(10), at(20)), (at(0), at(30))),
            Duration::minutes(10)
        );
        assert_eq!(overlap((at(0), at(10)), (at(20), at(30))), Duration::zero());
    }

    #[test]
    fn an_empty_report_is_recognisable() {
        assert!(ReconcileReport::default().is_empty());
        assert!(!ReconcileReport {
            meetings_linked: 1,
            ..Default::default()
        }
        .is_empty());
    }
}
