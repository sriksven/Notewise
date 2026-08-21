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
    ConnectorAccountRepository, Database, ExternalItemRepository, NewExternalItem,
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
    ExternalItemRepository::new(db).upsert(NewExternalItem {
        connector_id: connector_id.to_string(),
        external_id: item.external_id.clone(),
        url: item.url.clone(),
        title: item.title.clone(),
        remote_version: item.remote_version.clone(),
    })?;
    Ok(())
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
