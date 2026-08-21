//! The background passes: what happens without anybody pressing a button.
//!
//! # What was missing, and why it mattered
//!
//! Every connector in this product worked only when somebody pressed a button. Calendar events
//! arrived on Sync, a vault file was written on Mirror, an approved mail draft reached a mailbox on
//! Approve — and if any of those failed, the retry the outbox exists to schedule never happened.
//! The queue had a retry policy, a lease, an attempt counter, and nothing that ever looked at it
//! again.
//!
//! So: one pass in each direction. **Pull** asks every registered source for what is new. **Drain**
//! delivers whatever the outbox is holding, including the attempts a previous failure deferred.
//!
//! # And one that is not about connectors at all
//!
//! **Sweep** deletes retained audio past its retention policy. It lives here because it is the same
//! shape — a periodic pass over the workspace that wants its own connection — and because it had
//! exactly the same problem: `POST /v1/audio/sweep` existed, nothing called it, and its own log line
//! said "a later sweep will retry" about a later sweep that was never going to happen.
//!
//! That one is worse than a stalled queue. A user who set "keep audio for seven days" had audio kept
//! forever, and the setting that said otherwise was decoration.
//!
//! # Why these are not gated on a setting
//!
//! Connecting an account is the consent. A user who linked their calendar asked for their calendar
//! to be read, and a switch called "actually do the thing I connected" is a switch that exists to
//! be found in a support conversation. Nothing here reaches anything that is not connected, and
//! disconnecting stops it.
//!
//! The one thing that deliberately is gated is memory extraction, which reads transcripts to build a
//! durable profile — see [`crate::memory`] for why that is a different kind of ask.
//!
//! # Why every pass gets its own database connection
//!
//! `Importer::run` and `Dispatcher::drain` both borrow the database across their awaits, and
//! `Database` is `Send` but not `Sync` — so a future holding the shared guard across either is not
//! `Send`. It is also the right thing regardless: a vault write can stall for seconds inside an
//! iCloud folder while a sync client holds a lock, and the app's one connection should not be the
//! thing waiting.
//!
//! [`on_a_worker`] is that pattern in one place. It was written three times before this module
//! existed, which is two times too many for a piece of reasoning this specific.

use std::sync::Arc;
use std::time::Duration;

use crate::state::AppState;

type Shared = Arc<AppState>;

/// How often to ask sources for what is new.
///
/// Ten minutes, which is what AnythingLLM uses for the same job and for the same reason: a document
/// edited now needs to be searchable in minutes, not milliseconds, and a calendar event an hour away
/// does not care about the difference. Polling harder would spend a network round trip per source
/// per minute to learn nothing.
pub const PULL_TICK: Duration = Duration::from_secs(10 * 60);

/// How often to delete audio past its retention.
///
/// Hourly. Retention is measured in days, so this could be far slower — but a machine that is only
/// awake for an hour at a time would then never sweep at all, and the cost of asking is one indexed
/// query against a table of meetings.
pub const SWEEP_TICK: Duration = Duration::from_secs(60 * 60);

/// How often to try the outbox.
///
/// Much shorter, because this is a local queue and an empty one costs a single indexed query. The
/// interval only matters when something is waiting, and then it is the difference between a mail
/// draft appearing shortly and appearing in ten minutes.
pub const DRAIN_TICK: Duration = Duration::from_secs(30);

/// Run something with its own database connection, on a thread that may block.
///
/// The closure gets a connection and a current-thread runtime, which between them are what every
/// connector pass needs: see the module docs for why the shared connection cannot be used.
pub async fn on_a_worker<T, F>(state: &Shared, work: F) -> Result<T, String>
where
    F: FnOnce(&notewise_storage::Database, &tokio::runtime::Runtime) -> Result<T, String>
        + Send
        + 'static,
    T: Send + 'static,
{
    let Some(db_path) = state.db_path().map(std::path::Path::to_path_buf) else {
        return Err(
            "this needs a workspace stored on disk; this engine is in memory only".to_string(),
        );
    };

    tokio::task::spawn_blocking(move || {
        let db = notewise_storage::Database::open(&db_path).map_err(|e| e.to_string())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;

        work(&db, &runtime)
    })
    .await
    .map_err(|e| format!("the worker thread stopped: {e}"))?
}

/// Ask every registered source for what is new.
///
/// Answers `None` when nothing is connected, so the caller can tell "there was nothing to do" from
/// "it did nothing" — which is the difference between a quiet log and a bug report.
pub async fn pull_once(
    state: &Shared,
) -> Result<Option<notewise_connectors::ImportReport>, String> {
    let registry = state.connectors();
    if registry.source_ids().is_empty() {
        return Ok(None);
    }

    on_a_worker(state, move |db, runtime| {
        runtime
            .block_on(notewise_connectors::Importer::new(registry).run(db))
            .map(Some)
            .map_err(|e| e.to_string())
    })
    .await
}

/// Deliver whatever the outbox is holding.
///
/// Answers `None` when no sink is registered. A drain with nothing to deliver to would claim rows,
/// fail to find a connector for them, and dead-letter work that is only waiting for an account to be
/// reconnected — which is worse than not running.
pub async fn drain_once(
    state: &Shared,
) -> Result<Option<notewise_connectors::DispatchReport>, String> {
    let registry = state.connectors();
    if registry.sink_ids().is_empty() {
        return Ok(None);
    }

    on_a_worker(state, move |db, runtime| {
        let dispatcher = notewise_connectors::Dispatcher::new(
            registry,
            notewise_connectors::RetryPolicy::default(),
        );
        runtime
            .block_on(dispatcher.drain(db))
            .map(Some)
            .map_err(|e| e.to_string())
    })
    .await
}

/// Delete retained audio the policy says is past its time.
///
/// Answers how much went, so a quiet log stays quiet. Failures are reported by the sweep itself and
/// retried by the next one, which is now a sentence that is true.
pub async fn sweep_once(state: &Shared) -> Result<notewise_storage::SweepReport, String> {
    on_a_worker(state, |db, _| {
        let policy = notewise_storage::retention_policy(db);
        notewise_storage::sweep(db, policy, chrono::Utc::now()).map_err(|e| e.to_string())
    })
    .await
}

/// Start every pass.
///
/// Started with the server rather than with the router, like the scheduler and the calendar watcher:
/// `app` is also called by tests and by an embedder that only wants the route table, and neither
/// wants two background loops.
pub fn spawn(state: Shared) {
    let pulling = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(PULL_TICK).await;

            match pull_once(&pulling).await {
                Ok(Some(report)) if !report.is_empty() => {
                    tracing::info!(
                        pulled = report.pulled,
                        upserted = report.upserted,
                        failed = report.failures.len(),
                        "pulled from connected sources"
                    );
                }
                Ok(_) => {}
                // A pass that dies on one bad tick stops every source silently, which is the
                // failure mode a background loop is most likely to hide.
                Err(e) => tracing::warn!(error = %e, "a pull failed; continuing"),
            }
        }
    });

    let sweeping = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(SWEEP_TICK).await;

            match sweep_once(&sweeping).await {
                Ok(report) if report.deleted > 0 || !report.failed.is_empty() => {
                    tracing::info!(
                        deleted = report.deleted,
                        bytes_freed = report.bytes_freed,
                        failed = report.failed.len(),
                        "swept audio past its retention"
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "a retention sweep failed; continuing"),
            }
        }
    });

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(DRAIN_TICK).await;

            match drain_once(&state).await {
                Ok(Some(report)) if report.delivered > 0 || report.failed > 0 => {
                    tracing::info!(
                        delivered = report.delivered,
                        failed = report.failed,
                        "drained the outbox"
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "a delivery pass failed; continuing"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use notewise_ai_router::{Router as AiRouter, RouterConfig};
    use notewise_storage::Database;

    fn on_disk() -> (Shared, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("dir");
        let state = Arc::new(AppState::new(
            Database::open(dir.path().join("notewise.db")).expect("a workspace"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ));
        (state, dir)
    }

    /// Nothing connected is not an error, and the `None` is what lets a log stay quiet.
    #[tokio::test]
    async fn a_pass_with_nothing_connected_does_nothing_and_says_so() {
        let (state, _dir) = on_disk();

        assert!(pull_once(&state).await.expect("pulls").is_none());
        assert!(drain_once(&state).await.expect("drains").is_none());
    }

    /// The one thing `on_a_worker` refuses. An ephemeral engine cannot open a second connection to
    /// an in-memory database, and reporting that beats a confusing empty result.
    #[tokio::test]
    async fn an_in_memory_engine_is_told_it_cannot() {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ));

        let refused: Result<(), String> = on_a_worker(&state, |_, _| Ok(())).await;
        assert!(
            refused
                .as_ref()
                .err()
                .is_some_and(|e| e.contains("in memory")),
            "{refused:?}"
        );
    }

    /// The worker gets a real connection to the real workspace, not a fresh empty one.
    #[tokio::test]
    async fn the_worker_sees_the_same_workspace() {
        let (state, _dir) = on_disk();

        let meeting_id = {
            let db = state.db().await;
            notewise_storage::MeetingRepository::new(&db)
                .create(notewise_storage::NewMeeting {
                    project_id: None,
                    title: "Written by the main connection".into(),
                    source: notewise_storage::MeetingSource::Import,
                    started_at: chrono::Utc::now(),
                })
                .expect("a meeting")
                .id
        };

        let title = on_a_worker(&state, move |db, _| {
            notewise_storage::MeetingRepository::new(db)
                .get(meeting_id)
                .map(|meeting| meeting.title)
                .map_err(|e| e.to_string())
        })
        .await
        .expect("reads");

        assert_eq!(title, "Written by the main connection");
    }

    /// A closure that fails reports its own message rather than a thread panic.
    #[tokio::test]
    async fn a_failing_pass_reports_what_went_wrong() {
        let (state, _dir) = on_disk();

        let failed: Result<(), String> =
            on_a_worker(&state, |_, _| Err("the folder was unplugged".into())).await;
        assert_eq!(
            failed.as_ref().err().map(String::as_str),
            Some("the folder was unplugged")
        );
    }

    /// The drain actually delivers, through the whole stack: a queued row, a registered sink, a file
    /// on disk. This is the pass that did not exist, so it is worth proving end to end.
    #[tokio::test]
    async fn the_drain_delivers_what_the_outbox_is_holding() {
        let (state, _dir) = on_disk();
        let vault = tempfile::tempdir().expect("vault");

        let mut registry = notewise_connectors::ConnectorRegistry::new();
        registry.register_sink(Arc::new(notewise_connectors::VaultSink::new(vault.path())));
        state.set_connectors(registry);

        {
            let db = state.db().await;
            notewise_storage::OutboxRepository::new(&db)
                .enqueue(notewise_storage::NewOutboxEntry {
                    connector_id: notewise_connectors::VaultSink::ID.into(),
                    node_kind: "meeting".into(),
                    node_id: notewise_storage::Id::new(),
                    operation: notewise_connectors::Operation::Create.as_str().into(),
                    payload: serde_json::json!({
                        "title": "Platform standup",
                        "markdown": "# Platform standup"
                    })
                    .to_string(),
                    idempotency_key: "one".into(),
                })
                .expect("enqueues");
        }

        let report = drain_once(&state)
            .await
            .expect("drains")
            .expect("there was a sink");
        assert_eq!(report.delivered, 1, "{report:?}");

        let written = std::fs::read_dir(vault.path())
            .expect("reads the vault")
            .count();
        assert_eq!(written, 1, "the file should be there");
    }

    /// A delivery that failed is visible afterwards rather than lost.
    ///
    /// Named for what it proves. A missing vault folder is a configuration mistake rather than a
    /// blip, so the connector calls it permanent and the row dead-letters on the first attempt —
    /// which means this is not a test of the retry path, and pretending otherwise in the name would
    /// leave the retry path looking covered when it is not.
    #[tokio::test]
    async fn a_failed_delivery_is_recorded_where_it_can_be_seen() {
        let (state, _dir) = on_disk();
        let vault = tempfile::tempdir().expect("vault");
        let missing = vault.path().join("not-there-yet");

        // A vault pointed at a folder that does not exist. The write fails.
        let mut registry = notewise_connectors::ConnectorRegistry::new();
        registry.register_sink(Arc::new(notewise_connectors::VaultSink::new(&missing)));
        state.set_connectors(registry);

        {
            let db = state.db().await;
            notewise_storage::OutboxRepository::new(&db)
                .enqueue(notewise_storage::NewOutboxEntry {
                    connector_id: notewise_connectors::VaultSink::ID.into(),
                    node_kind: "meeting".into(),
                    node_id: notewise_storage::Id::new(),
                    operation: notewise_connectors::Operation::Create.as_str().into(),
                    payload: serde_json::json!({ "title": "Standup", "markdown": "# Standup" })
                        .to_string(),
                    idempotency_key: "one".into(),
                })
                .expect("enqueues");
        }

        let first = drain_once(&state).await.expect("drains").expect("a sink");
        assert_eq!(first.delivered, 0, "the folder is not there: {first:?}");

        // The folder appears, and a later pass finds the row still waiting.
        std::fs::create_dir_all(&missing).expect("creates the folder");

        // A missing vault folder is a configuration mistake rather than a blip, so the connector
        // reports it as permanent and the row dead-letters — which is the honest behaviour and the
        // reason this test asserts on the row rather than on a second delivery.
        let db = state.db().await;
        let failed = notewise_storage::OutboxRepository::new(&db)
            .list_failed(10)
            .expect("reads");
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(
            failed[0]
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("cannot write")),
            "{failed:?}"
        );
    }

    /// The intervals are the product decision here, so they are pinned.
    #[test]
    fn the_intervals_are_loose_for_pulling_and_tight_for_delivering() {
        assert!(PULL_TICK >= Duration::from_secs(5 * 60));
        assert!(DRAIN_TICK <= Duration::from_secs(60));
        assert!(DRAIN_TICK < PULL_TICK);
    }
    /// The pass that did not exist. A user who set "keep audio for seven days" had audio kept
    /// forever, and the setting that said otherwise was decoration.
    #[tokio::test]
    async fn a_sweep_runs_and_reports_what_it_removed() {
        let (state, _dir) = on_disk();

        // Nothing retained, so nothing to remove — and that is a report rather than an error.
        let report = sweep_once(&state).await.expect("sweeps");
        assert_eq!(report.deleted, 0);
        assert_eq!(report.bytes_freed, 0);
        assert!(report.failed.is_empty());
    }

    /// Hourly rather than daily, so a machine awake for an hour at a time still sweeps.
    #[test]
    fn the_sweep_runs_often_enough_for_a_machine_that_is_rarely_on() {
        assert!(SWEEP_TICK <= Duration::from_secs(2 * 60 * 60));
    }
}
