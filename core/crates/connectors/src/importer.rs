//! The inbound half of the connector seam.
//!
//! `Dispatcher` drains the outbox and pushes. Nothing pulled: `SourceConnector` has existed since
//! the seam was designed and had no engine behind it, so a source connector could be written and
//! never called. This is that engine.
//!
//! # Why the cursor advances last
//!
//! Identity is `(connector_id, external_id)` and `ExternalItemRepository::upsert` is keyed on it, so
//! reading the same item twice is a no-op. That makes re-reading cheap and skipping expensive — a
//! pull interrupted half way must resume from where it started, not from where it stopped. So the
//! cursor is written only after the batch has been persisted.
//!
//! # Why one failing source does not stop the others
//!
//! A calendar whose token expired should not prevent a document folder from importing. Each source
//! is attempted independently and its failure is recorded against it, exactly as `Dispatcher` treats
//! a failing sink.

use notewise_storage::{
    CalendarRepository, ConnectorAccountRepository, Database, DocumentRepository, EventStatus,
    ExternalItemRepository, Id, NewAttendee, NewCalendarEvent, NewDocument, NewExternalItem,
};

use crate::error::{ConnectorError, Result};
use crate::registry::ConnectorRegistry;
use crate::types::{Cursor, Inbound};

/// What one pass over every source moved.
#[derive(Debug, Default)]
pub struct ImportReport {
    /// Items the sources returned.
    pub pulled: usize,
    /// Items written or refreshed. Equal to `pulled` unless something failed to decode.
    pub upserted: usize,
    /// Sources that could not be read, and why. Their cursors are untouched.
    pub failures: Vec<(String, ConnectorError)>,
}

impl ImportReport {
    pub fn is_empty(&self) -> bool {
        self.pulled == 0 && self.failures.is_empty()
    }
}

/// Pulls from every registered source.
#[derive(Debug)]
pub struct Importer {
    registry: std::sync::Arc<ConnectorRegistry>,
}

impl Importer {
    /// Build an importer.
    ///
    /// Takes anything that becomes an `Arc<ConnectorRegistry>`, for the same reason
    /// [`crate::Dispatcher::new`] does: `api-server` keeps its registry shared so connecting an
    /// account in one window does not block a request in another, and rebuilding it per pull would
    /// re-read every credential from the keychain.
    pub fn new(registry: impl Into<std::sync::Arc<ConnectorRegistry>>) -> Self {
        Self {
            registry: registry.into(),
        }
    }

    pub fn registry(&self) -> &ConnectorRegistry {
        &self.registry
    }

    /// Pull every registered source once.
    pub async fn run(&self, db: &Database) -> Result<ImportReport> {
        let mut report = ImportReport::default();

        for id in self.registry.source_ids() {
            match self.pull_one(db, &id).await {
                Ok((pulled, upserted)) => {
                    report.pulled += pulled;
                    report.upserted += upserted;
                }
                Err(e) => {
                    tracing::warn!(connector = %id, error = %e, "a source could not be read");
                    report.failures.push((id, e));
                }
            }
        }

        Ok(report)
    }

    /// Pull one source, returning `(pulled, upserted)`.
    async fn pull_one(&self, db: &Database, id: &str) -> Result<(usize, usize)> {
        let source = self.registry.source(id)?;
        let accounts = ConnectorAccountRepository::new(db);

        let cursor = Cursor(accounts.get(id)?.and_then(|a| a.cursor));
        let batch = source.pull(cursor).await?;

        let mut upserted = 0;
        for item in &batch.items {
            match persist(db, id, item) {
                Ok(()) => upserted += 1,
                // One malformed item must not abandon the rest of the batch, and must not stop the
                // cursor advancing — otherwise a single bad row is re-read forever.
                Err(e) => tracing::warn!(
                    connector = %id,
                    external_id = %item.external_id,
                    error = %e,
                    "an inbound item could not be stored; skipping it"
                ),
            }
        }

        // Last, and only once everything above has committed.
        accounts.set_cursor(id, batch.next_cursor.as_deref())?;

        Ok((batch.items.len(), upserted))
    }
}

/// Record one inbound item's identity.
///
/// Only identity. What the item *is* — an event's start time, a document's body — belongs to
/// whichever crate understands that kind of thing, and lands in its own table keyed to this row.
/// `external_items` is the shared record that a thing exists elsewhere, for every connector.
fn persist(db: &Database, connector_id: &str, item: &Inbound) -> Result<()> {
    let external = ExternalItemRepository::new(db).upsert(NewExternalItem {
        connector_id: connector_id.to_string(),
        external_id: item.external_id.clone(),
        url: item.url.clone(),
        title: item.title.clone(),
        remote_version: item.remote_version.clone(),
    })?;
    land(db, connector_id, external.id, item)
}

/// Put what an inbound item *is* in the table that owns that kind of thing.
///
/// This step did not exist. Every source built its payload — a document's body, an event's times
/// and attendees, already normalised across providers — and the importer stored the identity and
/// dropped the rest, so `documents` and `calendar_events` stayed empty however many items a sync
/// reported. A watched folder made nothing searchable, and the calendar reconciliation and
/// join detection that read `calendar_events` had nothing to read. The sources and the
/// repositories are both here, so the step that joins them is too.
fn land(db: &Database, connector_id: &str, external_item_id: Id, item: &Inbound) -> Result<()> {
    match connector_id {
        crate::sources::Documents::ID => land_document(db, external_item_id, item),
        crate::sources::GoogleBridge::ID | crate::sources::MicrosoftGraph::ID => {
            land_event(db, connector_id, external_item_id, item)
        }
        _ => Ok(()),
    }
}

fn land_document(db: &Database, external_item_id: Id, item: &Inbound) -> Result<()> {
    let payload = &item.payload;
    let text = |field: &str| payload[field].as_str().map(str::to_string);

    let (Some(path), Some(body)) = (text("path"), text("body")) else {
        return Err(ConnectorError::Permanent(format!(
            "{} arrived without a path and a body",
            item.external_id
        )));
    };

    DocumentRepository::new(db).upsert(NewDocument {
        external_item_id,
        title: text("title")
            .or_else(|| item.title.clone())
            .unwrap_or_else(|| path.clone()),
        byte_size: payload["byte_size"]
            .as_i64()
            .unwrap_or_else(|| i64::try_from(body.len()).unwrap_or(i64::MAX)),
        modified_at: time(payload, "modified_at")
            .or(item.occurred_at)
            .unwrap_or_else(chrono::Utc::now),
        path,
        body,
    })?;
    Ok(())
}

fn land_event(
    db: &Database,
    connector_id: &str,
    external_item_id: Id,
    item: &Inbound,
) -> Result<()> {
    let payload = &item.payload;
    let text = |field: &str| payload[field].as_str().map(str::to_string);

    let (Some(starts_at), Some(ends_at)) = (time(payload, "starts_at"), time(payload, "ends_at"))
    else {
        return Err(ConnectorError::Permanent(format!(
            "event {} arrived without a start and an end",
            item.external_id
        )));
    };
    // Both sources normalise to these three; anything else is a source this code has not met.
    let status = text("status")
        .as_deref()
        .and_then(EventStatus::parse)
        .unwrap_or(EventStatus::Confirmed);

    let calendar = CalendarRepository::new(db);
    let event = calendar.upsert(NewCalendarEvent {
        external_item_id,
        calendar_id: text("calendar_id").unwrap_or_else(|| "primary".to_string()),
        provider_source: connector_id.to_string(),
        title: item.title.clone(),
        starts_at,
        ends_at,
        is_all_day: payload["is_all_day"].as_bool().unwrap_or(false),
        location: text("location"),
        join_url: text("join_url"),
        organizer_email: text("organizer_email"),
        recurrence_key: text("recurrence_key"),
        status,
    })?;

    let attendees: Vec<NewAttendee> = payload["attendees"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|attendee| {
            let email = attendee["email"].as_str()?.trim();
            (!email.is_empty()).then(|| NewAttendee {
                email: email.to_string(),
                display_name: attendee["display_name"].as_str().map(str::to_string),
                response_status: attendee["response_status"].as_str().map(str::to_string),
                is_organizer: attendee["is_organizer"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    calendar.replace_attendees(event.id, &attendees)?;
    Ok(())
}

/// An RFC 3339 timestamp from a payload field, as the sources write them.
fn time(payload: &serde_json::Value, field: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    payload[field]
        .as_str()
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::{Connector, SourceConnector};
    use crate::types::{Health, PullBatch};
    use async_trait::async_trait;
    use notewise_storage::AccountStatus;
    use std::sync::{Arc, Mutex};

    /// A source that returns what it was given, and records the cursor it was asked with.
    #[derive(Debug)]
    struct StubSource {
        id: String,
        batches: Mutex<Vec<PullBatch>>,
        seen: Arc<Mutex<Vec<Option<String>>>>,
        fail: bool,
    }

    impl StubSource {
        fn new(id: &str, batches: Vec<PullBatch>) -> Self {
            Self {
                id: id.into(),
                batches: Mutex::new(batches),
                seen: Arc::new(Mutex::new(Vec::new())),
                fail: false,
            }
        }

        fn failing(id: &str) -> Self {
            Self {
                id: id.into(),
                batches: Mutex::new(Vec::new()),
                seen: Arc::new(Mutex::new(Vec::new())),
                fail: true,
            }
        }
    }

    #[async_trait]
    impl Connector for StubSource {
        fn id(&self) -> &str {
            &self.id
        }
        fn display_name(&self) -> &str {
            "Stub"
        }
        fn is_local(&self) -> bool {
            true
        }
        async fn health(&self) -> Result<Health> {
            Ok(Health::Ok)
        }
    }

    #[async_trait]
    impl SourceConnector for StubSource {
        async fn pull(&self, since: Cursor) -> Result<PullBatch> {
            self.seen.lock().unwrap().push(since.0.clone());
            if self.fail {
                return Err(ConnectorError::Transient("the daemon is asleep".into()));
            }
            let mut batches = self.batches.lock().unwrap();
            Ok(if batches.is_empty() {
                PullBatch {
                    items: Vec::new(),
                    next_cursor: Cursor::start(),
                }
            } else {
                batches.remove(0)
            })
        }
    }

    fn item(external_id: &str, title: &str) -> Inbound {
        Inbound {
            external_id: external_id.into(),
            url: None,
            title: Some(title.into()),
            remote_version: None,
            occurred_at: None,
            payload: serde_json::json!({}),
        }
    }

    fn batch(items: Vec<Inbound>, cursor: Option<&str>) -> PullBatch {
        PullBatch {
            items,
            next_cursor: Cursor(cursor.map(str::to_string)),
        }
    }

    fn db_with(connector_id: &str) -> Database {
        let db = Database::open_in_memory().expect("in-memory db");
        ConnectorAccountRepository::new(&db)
            .connect(connector_id, Some("target"), &[])
            .expect("connect");
        db
    }

    fn importer(source: StubSource) -> Importer {
        let mut registry = ConnectorRegistry::new();
        registry.register_source(Arc::new(source));
        Importer::new(registry)
    }

    /// The whole path a watched folder takes, with the real source: a file in the folder has to
    /// end up findable. Before the landing step existed, the sync reported it upserted and the
    /// `documents` table — the one search and retrieval read — stayed empty.
    #[tokio::test]
    async fn a_watched_folder_becomes_searchable_documents() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("plan.md"),
            "# Plan\n\nThe zebra migration ships Friday.\n",
        )
        .expect("write");

        let db = db_with(crate::sources::Documents::ID);
        let mut registry = ConnectorRegistry::new();
        registry.register_source(Arc::new(crate::sources::Documents::new(dir.path())));
        let report = Importer::new(registry).run(&db).await.expect("run");
        assert_eq!(report.upserted, 1, "{report:?}");

        let documents = DocumentRepository::new(&db).list().expect("list");
        assert_eq!(documents.len(), 1);
        assert!(documents[0].body.contains("zebra"));

        let hits = notewise_storage::SearchRepository::new(&db)
            .search("zebra", 10)
            .expect("search");
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].entity_kind, "document");
    }

    /// Both calendar sources normalise events to one payload; it has to land as an event with its
    /// attendees, which is what reconciliation and join detection read.
    #[tokio::test]
    async fn a_calendar_event_lands_with_its_attendees() {
        let db = db_with(crate::sources::GoogleBridge::ID);
        let mut event = item("evt-1", "Platform standup");
        event.payload = serde_json::json!({
            "calendar_id": "primary",
            "starts_at": "2026-10-19T09:00:00+00:00",
            "ends_at": "2026-10-19T09:30:00+00:00",
            "is_all_day": false,
            "location": null,
            "join_url": "https://meet.google.com/abc-defg-hij",
            "organizer_email": "dana@example.com",
            "recurrence_key": "standup-series",
            "status": "confirmed",
            "attendees": [
                {"email": "dana@example.com", "display_name": "Dana", "response_status": "accepted", "is_organizer": true},
                {"email": "sam@example.com", "display_name": null, "response_status": null, "is_organizer": false},
                {"email": "  ", "display_name": "nobody", "response_status": null, "is_organizer": false}
            ],
        });
        let source = StubSource::new(
            crate::sources::GoogleBridge::ID,
            vec![batch(vec![event], None)],
        );

        let report = importer(source).run(&db).await.expect("run");
        assert_eq!(report.upserted, 1, "{report:?}");

        let external = ExternalItemRepository::new(&db)
            .find(crate::sources::GoogleBridge::ID, "evt-1")
            .expect("find")
            .expect("identity stored");
        let calendar = CalendarRepository::new(&db);
        let landed = calendar
            .by_external_item(external.id)
            .expect("read")
            .expect("the event must land, not just its identity");
        assert_eq!(landed.title.as_deref(), Some("Platform standup"));
        assert_eq!(
            landed.join_url.as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
        assert_eq!(landed.recurrence_key.as_deref(), Some("standup-series"));
        assert_eq!(landed.provider_source, "google");

        let attendees = calendar.attendees(landed.id).expect("attendees");
        assert_eq!(attendees.len(), 2, "a blank address is not an attendee");
        assert!(attendees
            .iter()
            .any(|a| a.email == "dana@example.com" && a.is_organizer));
    }

    /// One malformed event must not cost the batch — the existing rule, now that landing can fail.
    #[tokio::test]
    async fn an_event_without_times_is_skipped_not_fatal() {
        let db = db_with(crate::sources::MicrosoftGraph::ID);
        let mut broken = item("evt-broken", "No times");
        broken.payload = serde_json::json!({ "status": "confirmed" });
        let mut fine = item("evt-fine", "Has times");
        fine.payload = serde_json::json!({
            "starts_at": "2026-10-19T09:00:00Z",
            "ends_at": "2026-10-19T10:00:00Z",
        });
        let source = StubSource::new(
            crate::sources::MicrosoftGraph::ID,
            vec![batch(vec![broken, fine], None)],
        );

        let report = importer(source).run(&db).await.expect("run");
        assert_eq!((report.pulled, report.upserted), (2, 1));
    }

    #[tokio::test]
    async fn an_empty_registry_imports_nothing() {
        let db = Database::open_in_memory().expect("db");
        let report = Importer::new(ConnectorRegistry::new())
            .run(&db)
            .await
            .expect("run");
        assert!(report.is_empty());
    }

    #[tokio::test]
    async fn pulled_items_become_external_items() {
        let db = db_with("stub");
        let source = StubSource::new(
            "stub",
            vec![batch(
                vec![item("a", "First"), item("b", "Second")],
                Some("page-2"),
            )],
        );

        let report = importer(source).run(&db).await.expect("run");
        assert_eq!((report.pulled, report.upserted), (2, 2));

        let repo = ExternalItemRepository::new(&db);
        assert!(repo.find("stub", "a").expect("find").is_some());
        assert_eq!(
            repo.find("stub", "b")
                .expect("find")
                .unwrap()
                .title
                .as_deref(),
            Some("Second")
        );
    }

    #[tokio::test]
    async fn the_cursor_advances_and_is_used_next_time() {
        let db = db_with("stub");
        let source = StubSource::new(
            "stub",
            vec![
                batch(vec![item("a", "First")], Some("page-2")),
                batch(vec![item("b", "Second")], Some("page-3")),
            ],
        );
        let seen = Arc::clone(&source.seen);
        let importer = importer(source);

        importer.run(&db).await.expect("first");
        importer.run(&db).await.expect("second");

        assert_eq!(
            *seen.lock().unwrap(),
            vec![None, Some("page-2".to_string())],
            "the second pull must resume from where the first left off"
        );
        assert_eq!(
            ConnectorAccountRepository::new(&db)
                .get("stub")
                .expect("account")
                .unwrap()
                .cursor
                .as_deref(),
            Some("page-3")
        );
    }

    /// Re-reading is cheap; skipping is not. An interrupted pull must resume from where it started.
    #[tokio::test]
    async fn a_failed_pull_leaves_the_cursor_alone() {
        let db = db_with("stub");
        ConnectorAccountRepository::new(&db)
            .set_cursor("stub", Some("page-1"))
            .expect("cursor");

        let report = importer(StubSource::failing("stub"))
            .run(&db)
            .await
            .expect("run");

        assert_eq!(report.failures.len(), 1);
        assert_eq!(
            ConnectorAccountRepository::new(&db)
                .get("stub")
                .expect("account")
                .unwrap()
                .cursor
                .as_deref(),
            Some("page-1"),
            "a failed pull must not advance past what it never read"
        );
    }

    #[tokio::test]
    async fn reading_the_same_item_twice_does_not_duplicate_it() {
        let db = db_with("stub");
        let source = StubSource::new(
            "stub",
            vec![
                batch(vec![item("a", "First")], None),
                batch(vec![item("a", "First, renamed")], None),
            ],
        );
        let importer = importer(source);

        importer.run(&db).await.expect("first");
        importer.run(&db).await.expect("second");

        let found = ExternalItemRepository::new(&db)
            .find("stub", "a")
            .expect("find")
            .expect("present");
        assert_eq!(
            found.title.as_deref(),
            Some("First, renamed"),
            "the second read should refresh the row rather than add another"
        );
    }

    /// A calendar whose token expired must not stop a document folder importing.
    #[tokio::test]
    async fn one_failing_source_does_not_stop_the_others() {
        let db = Database::open_in_memory().expect("db");
        let accounts = ConnectorAccountRepository::new(&db);
        accounts.connect("broken", Some("x"), &[]).expect("connect");
        accounts
            .connect("working", Some("y"), &[])
            .expect("connect");

        let mut registry = ConnectorRegistry::new();
        registry.register_source(Arc::new(StubSource::failing("broken")));
        registry.register_source(Arc::new(StubSource::new(
            "working",
            vec![batch(vec![item("a", "First")], None)],
        )));

        let report = Importer::new(registry).run(&db).await.expect("run");

        assert_eq!(report.pulled, 1, "the healthy source still imported");
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].0, "broken");
    }

    #[tokio::test]
    async fn a_source_with_nothing_new_is_not_a_failure() {
        let db = db_with("stub");
        let report = importer(StubSource::new("stub", vec![]))
            .run(&db)
            .await
            .expect("run");

        assert!(report.is_empty());
        assert!(report.failures.is_empty());
    }

    /// A source registered but whose account was disabled still has a cursor to respect; the
    /// registry is what decides whether it is pulled at all, and `build_registry` already skips a
    /// disabled account. This checks the importer does not invent one.
    #[tokio::test]
    async fn an_account_that_was_never_connected_still_pulls_from_the_start() {
        let db = Database::open_in_memory().expect("db");
        let source = StubSource::new("stub", vec![batch(vec![item("a", "First")], Some("p2"))]);
        let seen = Arc::clone(&source.seen);

        let report = importer(source).run(&db).await.expect("run");

        assert_eq!(report.pulled, 1);
        assert_eq!(*seen.lock().unwrap(), vec![None]);
        let _ = AccountStatus::Connected;
    }
}
