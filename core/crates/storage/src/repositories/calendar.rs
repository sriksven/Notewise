//! Calendar events and their attendees.
//!
//! An event is detail *owned by* an `external_items` row — it cascades from it, because the event
//! only exists as a record of something in a calendar elsewhere. The association between a meeting
//! and its event is a graph edge, not a column here; that split is what stops a migration the first
//! time an event links to something other than a meeting.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row};

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;

/// Whether the event is still happening, per the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventStatus {
    Confirmed,
    Tentative,
    /// Cancelled remotely. Kept rather than deleted, so a meeting already linked to it does not
    /// lose its provenance when somebody cancels the invite afterwards.
    Cancelled,
}

impl EventStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventStatus::Confirmed => "confirmed",
            EventStatus::Tentative => "tentative",
            EventStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "confirmed" => EventStatus::Confirmed,
            "tentative" => EventStatus::Tentative,
            "cancelled" => EventStatus::Cancelled,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    pub id: Id,
    pub external_item_id: Id,
    pub calendar_id: String,
    /// Which account it came from — `google`, `outlook`, `exchange`, `icloud`, `other`.
    pub provider_source: String,
    pub title: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub is_all_day: bool,
    pub location: Option<String>,
    pub join_url: Option<String>,
    pub organizer_email: Option<String>,
    /// The provider's own recurrence identifier, normalised.
    pub recurrence_key: Option<String>,
    pub status: EventStatus,
}

#[derive(Debug, Clone)]
pub struct NewCalendarEvent {
    pub external_item_id: Id,
    pub calendar_id: String,
    pub provider_source: String,
    pub title: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub is_all_day: bool,
    pub location: Option<String>,
    pub join_url: Option<String>,
    pub organizer_email: Option<String>,
    pub recurrence_key: Option<String>,
    pub status: EventStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attendee {
    pub id: Id,
    pub calendar_event_id: Id,
    pub email: String,
    pub display_name: Option<String>,
    pub response_status: Option<String>,
    pub is_organizer: bool,
    /// The person this attendee resolves to, once reconciliation has matched them.
    pub person_id: Option<Id>,
}

#[derive(Debug, Clone)]
pub struct NewAttendee {
    pub email: String,
    pub display_name: Option<String>,
    pub response_status: Option<String>,
    pub is_organizer: bool,
}

#[derive(Debug)]
pub struct CalendarRepository<'a> {
    db: &'a Database,
}

impl<'a> CalendarRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Insert or refresh the event for an external item.
    ///
    /// Keyed on `external_item_id`, which is unique: re-reading the same event updates it rather
    /// than adding a second copy, which is what makes the importer's re-reads harmless.
    pub fn upsert(&self, new: NewCalendarEvent) -> Result<CalendarEvent> {
        let id = Id::new();
        self.db.conn().execute(
            "INSERT INTO calendar_events
                (id, external_item_id, calendar_id, provider_source, title, starts_at, ends_at,
                 is_all_day, location, join_url, organizer_email, recurrence_key, status,
                 updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(external_item_id) DO UPDATE SET
                calendar_id      = excluded.calendar_id,
                provider_source  = excluded.provider_source,
                title            = excluded.title,
                starts_at        = excluded.starts_at,
                ends_at          = excluded.ends_at,
                is_all_day       = excluded.is_all_day,
                location         = excluded.location,
                join_url         = excluded.join_url,
                organizer_email  = excluded.organizer_email,
                recurrence_key   = excluded.recurrence_key,
                status           = excluded.status,
                updated_at       = excluded.updated_at",
            rusqlite::params![
                id,
                new.external_item_id,
                new.calendar_id,
                new.provider_source,
                new.title,
                new.starts_at,
                new.ends_at,
                new.is_all_day,
                new.location,
                new.join_url,
                new.organizer_email,
                new.recurrence_key,
                new.status.as_str(),
                Utc::now(),
            ],
        )?;

        self.by_external_item(new.external_item_id)?
            .ok_or_else(|| StorageError::not_found("CalendarEvent", new.external_item_id))
    }

    pub fn by_external_item(&self, external_item_id: Id) -> Result<Option<CalendarEvent>> {
        let conn = self.db.conn();
        let row = conn
            .query_row(
                &format!("{SELECT_EVENT} WHERE external_item_id = ?1"),
                rusqlite::params![external_item_id],
                map_event,
            )
            .optional()?;
        Ok(row)
    }

    pub fn get(&self, id: Id) -> Result<CalendarEvent> {
        let conn = self.db.conn();
        conn.query_row(
            &format!("{SELECT_EVENT} WHERE id = ?1"),
            rusqlite::params![id],
            map_event,
        )
        .optional()?
        .ok_or_else(|| StorageError::not_found("CalendarEvent", id))
    }

    /// Events overlapping a window, excluding cancelled ones.
    ///
    /// Overlap rather than containment: a meeting that ran long still belongs to the event it
    /// started in.
    pub fn overlapping(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CalendarEvent>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(&format!(
            "{SELECT_EVENT} WHERE status <> 'cancelled' AND starts_at < ?2 AND ends_at > ?1
              ORDER BY starts_at"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params![from, to], map_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Events between two instants, for showing what is coming up.
    pub fn between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<CalendarEvent>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(&format!(
            "{SELECT_EVENT} WHERE status <> 'cancelled' AND starts_at >= ?1 AND starts_at <= ?2
              ORDER BY starts_at"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params![from, to], map_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Events not yet linked to any meeting.
    ///
    /// Unlinked is the normal state for anything in the future, so this is a work queue rather than
    /// a problem list.
    pub fn unlinked(&self, limit: usize) -> Result<Vec<CalendarEvent>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(&format!(
            "{SELECT_EVENT} e
              WHERE e.status <> 'cancelled'
                AND NOT EXISTS (
                    SELECT 1 FROM edges
                     WHERE edge_kind = 'synced_to'
                       AND to_kind = 'external_item'
                       AND to_id = e.external_item_id
                )
              ORDER BY e.starts_at DESC LIMIT ?1"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64], map_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn by_recurrence_key(&self, key: &str) -> Result<Vec<CalendarEvent>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(&format!(
            "{SELECT_EVENT} WHERE recurrence_key = ?1 ORDER BY starts_at"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params![key], map_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Replace an event's attendee list.
    ///
    /// Delete-and-insert rather than a diff: an attendee removed from an invitation has to
    /// disappear, and computing the difference against the remote list is more code than clearing
    /// the rows. Any `person_id` already resolved is preserved by matching on email.
    pub fn replace_attendees(&self, event_id: Id, attendees: &[NewAttendee]) -> Result<()> {
        let conn = self.db.conn();

        // Keep what reconciliation has already worked out, so re-importing an event does not undo
        // the person matching and force it to be redone.
        let mut resolved = std::collections::HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT email, person_id FROM calendar_attendees
                  WHERE calendar_event_id = ?1 AND person_id IS NOT NULL",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![event_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Id>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (email, person) in rows {
                resolved.insert(email, person);
            }
        }

        conn.execute(
            "DELETE FROM calendar_attendees WHERE calendar_event_id = ?1",
            rusqlite::params![event_id],
        )?;

        for attendee in attendees {
            conn.execute(
                "INSERT OR IGNORE INTO calendar_attendees
                    (id, calendar_event_id, email, display_name, response_status, is_organizer,
                     person_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    Id::new(),
                    event_id,
                    attendee.email,
                    attendee.display_name,
                    attendee.response_status,
                    attendee.is_organizer,
                    resolved.get(&attendee.email),
                ],
            )?;
        }

        Ok(())
    }

    pub fn attendees(&self, event_id: Id) -> Result<Vec<Attendee>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, calendar_event_id, email, display_name, response_status, is_organizer,
                    person_id
               FROM calendar_attendees WHERE calendar_event_id = ?1 ORDER BY is_organizer DESC, email",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![event_id], |r| {
                Ok(Attendee {
                    id: r.get(0)?,
                    calendar_event_id: r.get(1)?,
                    email: r.get(2)?,
                    display_name: r.get(3)?,
                    response_status: r.get(4)?,
                    is_organizer: r.get(5)?,
                    person_id: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Attendees across all events that have not been matched to a person yet.
    pub fn unresolved_attendees(&self, limit: usize) -> Result<Vec<Attendee>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, calendar_event_id, email, display_name, response_status, is_organizer,
                    person_id
               FROM calendar_attendees
              WHERE person_id IS NULL AND TRIM(email) <> ''
              LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |r| {
                Ok(Attendee {
                    id: r.get(0)?,
                    calendar_event_id: r.get(1)?,
                    email: r.get(2)?,
                    display_name: r.get(3)?,
                    response_status: r.get(4)?,
                    is_organizer: r.get(5)?,
                    person_id: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_attendee_person(&self, attendee_id: Id, person_id: Id) -> Result<()> {
        self.db.conn().execute(
            "UPDATE calendar_attendees SET person_id = ?2 WHERE id = ?1",
            rusqlite::params![attendee_id, person_id],
        )?;
        Ok(())
    }
}

const SELECT_EVENT: &str = "SELECT id, external_item_id, calendar_id, provider_source, title, \
     starts_at, ends_at, is_all_day, location, join_url, organizer_email, recurrence_key, status \
     FROM calendar_events";

fn map_event(row: &Row<'_>) -> rusqlite::Result<CalendarEvent> {
    let status: String = row.get(12)?;
    Ok(CalendarEvent {
        id: row.get(0)?,
        external_item_id: row.get(1)?,
        calendar_id: row.get(2)?,
        provider_source: row.get(3)?,
        title: row.get(4)?,
        starts_at: row.get(5)?,
        ends_at: row.get(6)?,
        is_all_day: row.get(7)?,
        location: row.get(8)?,
        join_url: row.get(9)?,
        organizer_email: row.get(10)?,
        recurrence_key: row.get(11)?,
        status: EventStatus::parse(&status).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                12,
                rusqlite::types::Type::Text,
                format!("unknown event status '{status}'").into(),
            )
        })?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::{ExternalItemRepository, NewExternalItem};
    use chrono::Duration;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn external(db: &Database, id: &str) -> Id {
        ExternalItemRepository::new(db)
            .upsert(NewExternalItem {
                connector_id: "google".into(),
                external_id: id.into(),
                url: None,
                title: Some("Standup".into()),
                remote_version: None,
            })
            .expect("external item")
            .id
    }

    fn at(hours: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + hours * 3600, 0).expect("timestamp")
    }

    fn event(item: Id, starts: i64, ends: i64) -> NewCalendarEvent {
        NewCalendarEvent {
            external_item_id: item,
            calendar_id: "primary".into(),
            provider_source: "google".into(),
            title: Some("Standup".into()),
            starts_at: at(starts),
            ends_at: at(ends),
            is_all_day: false,
            location: None,
            join_url: Some("https://meet.example/abc".into()),
            organizer_email: Some("me@example.com".into()),
            recurrence_key: None,
            status: EventStatus::Confirmed,
        }
    }

    #[test]
    fn an_event_round_trips() {
        let db = db();
        let item = external(&db, "evt-1");
        let repo = CalendarRepository::new(&db);

        let made = repo.upsert(event(item, 0, 1)).expect("upsert");
        assert_eq!(repo.get(made.id).expect("get"), made);
        assert_eq!(made.join_url.as_deref(), Some("https://meet.example/abc"));
    }

    /// The property the importer's re-reads depend on.
    #[test]
    fn re_importing_the_same_event_updates_rather_than_duplicating() {
        let db = db();
        let item = external(&db, "evt-1");
        let repo = CalendarRepository::new(&db);

        repo.upsert(event(item, 0, 1)).expect("first");
        let mut moved = event(item, 2, 3);
        moved.title = Some("Standup, moved".into());
        let second = repo.upsert(moved).expect("second");

        assert_eq!(second.starts_at, at(2));
        assert_eq!(second.title.as_deref(), Some("Standup, moved"));
        assert_eq!(
            repo.overlapping(at(-100), at(100))
                .expect("overlapping")
                .len(),
            1,
            "one event, updated — not two"
        );
    }

    #[test]
    fn deleting_the_external_item_takes_the_event_with_it() {
        let db = db();
        let item = external(&db, "evt-1");
        CalendarRepository::new(&db)
            .upsert(event(item, 0, 1))
            .expect("upsert");

        db.conn()
            .execute(
                "DELETE FROM external_items WHERE id = ?1",
                rusqlite::params![item],
            )
            .expect("delete");

        assert!(CalendarRepository::new(&db)
            .by_external_item(item)
            .expect("query")
            .is_none());
    }

    /// A meeting that ran long still belongs to the event it started in, so overlap is what
    /// matters rather than containment.
    #[test]
    fn overlap_finds_a_partial_intersection() {
        let db = db();
        let repo = CalendarRepository::new(&db);
        repo.upsert(event(external(&db, "a"), 10, 11))
            .expect("event");

        assert_eq!(
            repo.overlapping(at(10) + Duration::minutes(30), at(12))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(repo.overlapping(at(12), at(13)).unwrap().len(), 0);
        // Touching at the boundary is not overlapping.
        assert_eq!(repo.overlapping(at(11), at(12)).unwrap().len(), 0);
    }

    #[test]
    fn a_cancelled_event_is_kept_but_not_offered() {
        let db = db();
        let repo = CalendarRepository::new(&db);
        let item = external(&db, "a");
        let mut cancelled = event(item, 10, 11);
        cancelled.status = EventStatus::Cancelled;
        let made = repo.upsert(cancelled).expect("upsert");

        assert!(repo.overlapping(at(9), at(12)).unwrap().is_empty());
        assert!(repo.unlinked(10).unwrap().is_empty());
        assert!(
            repo.get(made.id).is_ok(),
            "kept, so a meeting already linked to it does not lose its provenance"
        );
    }

    #[test]
    fn attendees_are_replaced_wholesale() {
        let db = db();
        let repo = CalendarRepository::new(&db);
        let made = repo
            .upsert(event(external(&db, "a"), 0, 1))
            .expect("upsert");

        repo.replace_attendees(
            made.id,
            &[
                NewAttendee {
                    email: "me@example.com".into(),
                    display_name: Some("Me".into()),
                    response_status: Some("accepted".into()),
                    is_organizer: true,
                },
                NewAttendee {
                    email: "priya@example.com".into(),
                    display_name: Some("Priya".into()),
                    response_status: Some("accepted".into()),
                    is_organizer: false,
                },
            ],
        )
        .expect("attendees");
        assert_eq!(repo.attendees(made.id).unwrap().len(), 2);

        // Priya was removed from the invitation, so she has to disappear.
        repo.replace_attendees(
            made.id,
            &[NewAttendee {
                email: "me@example.com".into(),
                display_name: Some("Me".into()),
                response_status: Some("accepted".into()),
                is_organizer: true,
            }],
        )
        .expect("replace");

        let left = repo.attendees(made.id).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].email, "me@example.com");
    }

    /// Re-importing an event must not undo the person matching and force it to be redone.
    #[test]
    fn a_resolved_person_survives_a_reimport() {
        let db = db();
        let repo = CalendarRepository::new(&db);
        let made = repo
            .upsert(event(external(&db, "a"), 0, 1))
            .expect("upsert");

        let attendee = NewAttendee {
            email: "priya@example.com".into(),
            display_name: Some("Priya".into()),
            response_status: None,
            is_organizer: false,
        };
        repo.replace_attendees(made.id, std::slice::from_ref(&attendee))
            .expect("first");

        let person = crate::repositories::PersonRepository::new(&db)
            .create(crate::repositories::NewPerson {
                display_name: "Priya".into(),
                email: Some("priya@example.com".into()),
            })
            .expect("person");
        let existing = repo.attendees(made.id).unwrap();
        repo.set_attendee_person(existing[0].id, person.id)
            .expect("resolve");

        repo.replace_attendees(made.id, std::slice::from_ref(&attendee))
            .expect("reimport");

        assert_eq!(
            repo.attendees(made.id).unwrap()[0].person_id,
            Some(person.id),
            "the match has to survive, or every import redoes the work"
        );
    }

    #[test]
    fn recurring_instances_are_findable_by_their_key() {
        let db = db();
        let repo = CalendarRepository::new(&db);

        for (n, id) in ["a", "b"].iter().enumerate() {
            let mut e = event(external(&db, id), n as i64 * 24, n as i64 * 24 + 1);
            e.recurrence_key = Some("weekly-standup".into());
            repo.upsert(e).expect("upsert");
        }

        assert_eq!(repo.by_recurrence_key("weekly-standup").unwrap().len(), 2);
        assert!(repo.by_recurrence_key("something-else").unwrap().is_empty());
    }

    #[test]
    fn unresolved_attendees_are_offered_for_matching() {
        let db = db();
        let repo = CalendarRepository::new(&db);
        let made = repo
            .upsert(event(external(&db, "a"), 0, 1))
            .expect("upsert");
        repo.replace_attendees(
            made.id,
            &[NewAttendee {
                email: "priya@example.com".into(),
                display_name: Some("Priya".into()),
                response_status: None,
                is_organizer: false,
            }],
        )
        .expect("attendees");

        let pending = repo.unresolved_attendees(10).unwrap();
        assert_eq!(pending.len(), 1);

        let person = crate::repositories::PersonRepository::new(&db)
            .create(crate::repositories::NewPerson {
                display_name: "Priya".into(),
                email: Some("priya@example.com".into()),
            })
            .expect("person");
        repo.set_attendee_person(pending[0].id, person.id)
            .expect("resolve");

        assert!(repo.unresolved_attendees(10).unwrap().is_empty());
    }

    #[test]
    fn statuses_round_trip() {
        for status in [
            EventStatus::Confirmed,
            EventStatus::Tentative,
            EventStatus::Cancelled,
        ] {
            assert_eq!(EventStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(EventStatus::parse("maybe"), None);
    }
}
