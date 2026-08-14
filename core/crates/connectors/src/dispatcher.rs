//! Draining the outbox.
//!
//! The dispatcher is the only thing that calls a sink. It classifies each failure into
//! retry-with-backoff or dead-letter, and on success records the resulting artifact as an
//! `external_item` joined to its source node by a `synced_to` edge — so a pushed action item
//! is reachable from `find_related` rather than through a connector-specific lookup.

use chrono::{Duration, Utc};
use notewise_graph::{EdgeKind, Graph, NodeKind, NodeRef};
use notewise_storage::{
    Database, ExternalItemRepository, NewExternalItem, OutboxRecord, OutboxRepository,
};

use crate::error::{ConnectorError, Result};
use crate::registry::ConnectorRegistry;
use crate::types::{ExternalRef, Operation, Outbound};

/// How the dispatcher schedules retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    /// How long a claimed row stays leased before another dispatcher may take it.
    pub lease: Duration,
    /// Rows claimed per drain.
    pub batch_size: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::seconds(30),
            max_delay: Duration::hours(6),
            lease: Duration::minutes(5),
            batch_size: 25,
        }
    }
}

impl RetryPolicy {
    /// Exponential backoff, capped. `attempts` is the count *before* this failure.
    fn delay_for(&self, attempts: u32) -> Duration {
        let factor = 2_i64.saturating_pow(attempts.min(16));
        let delay = self.base_delay * factor as i32;
        if delay > self.max_delay {
            self.max_delay
        } else {
            delay
        }
    }
}

/// What one drain did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DispatchReport {
    pub delivered: u32,
    /// Failed this time, but scheduled to try again.
    pub deferred: u32,
    /// Given up on and dead-lettered.
    pub failed: u32,
}

#[derive(Debug)]
pub struct Dispatcher {
    registry: ConnectorRegistry,
    policy: RetryPolicy,
}

impl Dispatcher {
    pub fn new(registry: ConnectorRegistry, policy: RetryPolicy) -> Self {
        Self { registry, policy }
    }

    pub fn registry(&self) -> &ConnectorRegistry {
        &self.registry
    }

    /// Claim and deliver one batch.
    ///
    /// Returns `Ok` even when individual deliveries fail — a failed push is recorded on its
    /// row, not propagated, because one broken connector must not stop the others.
    pub async fn drain(&self, db: &Database) -> Result<DispatchReport> {
        let claimed =
            OutboxRepository::new(db).claim_ready(self.policy.batch_size, self.policy.lease)?;

        let mut report = DispatchReport::default();
        for row in claimed {
            match self.deliver(db, &row).await {
                Ok(()) => report.delivered += 1,
                Err(err) => {
                    if self.record_failure(db, &row, &err)? {
                        report.deferred += 1;
                    } else {
                        report.failed += 1;
                    }
                }
            }
        }

        Ok(report)
    }

    async fn deliver(&self, db: &Database, row: &OutboxRecord) -> Result<()> {
        let sink = self.registry.sink(&row.connector_id)?;

        let operation = Operation::parse(&row.operation).ok_or_else(|| {
            ConnectorError::Permanent(format!("unknown operation '{}'", row.operation))
        })?;
        let node_kind = NodeKind::parse(&row.node_kind).ok_or_else(|| {
            ConnectorError::Permanent(format!("unknown node kind '{}'", row.node_kind))
        })?;

        let node = NodeRef::new(node_kind, row.node_id);

        let outbound = Outbound {
            node_kind: row.node_kind.clone(),
            node_id: row.node_id,
            operation,
            payload: serde_json::from_str(&row.payload)?,
            existing: self.existing_ref(db, node, &row.connector_id)?,
        };

        let reference = sink.push(&outbound).await?;
        self.record_success(db, row, node_kind, &reference)?;
        OutboxRepository::new(db).complete(row.id)?;
        Ok(())
    }

    /// The artifact this node already has in this connector, if any.
    ///
    /// Without this, `Outbound::existing` would always be `None` and a second push would file
    /// a second ticket — the exact failure the outbox exists to prevent, reintroduced one
    /// layer up. The outbox stops a *retry* from duplicating; this stops a genuinely new
    /// enqueue for an already-synced node from duplicating.
    fn existing_ref(
        &self,
        db: &Database,
        node: NodeRef,
        connector_id: &str,
    ) -> Result<Option<ExternalRef>> {
        let items = ExternalItemRepository::new(db);

        for related in Graph::new(db).related(node, 1)? {
            if related.node.kind != NodeKind::ExternalItem || related.via != EdgeKind::SyncedTo {
                continue;
            }

            let item = items.get(related.node.id)?;
            if item.connector_id == connector_id {
                return Ok(Some(ExternalRef {
                    external_id: item.external_id,
                    url: item.url,
                    title: item.title,
                    remote_version: item.remote_version,
                }));
            }
        }

        Ok(None)
    }

    fn record_success(
        &self,
        db: &Database,
        row: &OutboxRecord,
        node_kind: NodeKind,
        reference: &ExternalRef,
    ) -> Result<()> {
        let item = ExternalItemRepository::new(db).upsert(NewExternalItem {
            connector_id: row.connector_id.clone(),
            external_id: reference.external_id.clone(),
            url: reference.url.clone(),
            title: reference.title.clone(),
            remote_version: reference.remote_version.clone(),
        })?;

        Graph::new(db).connect(
            NodeRef::new(node_kind, row.node_id),
            EdgeKind::SyncedTo,
            NodeRef::new(NodeKind::ExternalItem, item.id),
        )?;

        Ok(())
    }

    /// Record a failure. Returns `true` if another attempt was scheduled.
    fn record_failure(
        &self,
        db: &Database,
        row: &OutboxRecord,
        err: &ConnectorError,
    ) -> Result<bool> {
        let outbox = OutboxRepository::new(db);
        let attempts_after = row.attempts + 1;

        if !err.is_retryable() || attempts_after >= self.policy.max_attempts {
            tracing::warn!(
                connector = %row.connector_id,
                outbox_id = %row.id,
                error = %err,
                "giving up on delivery"
            );
            outbox.dead_letter(row.id, &err.to_string())?;
            return Ok(false);
        }

        let delay = err
            .retry_after()
            .and_then(|d| Duration::from_std(d).ok())
            .unwrap_or_else(|| self.policy.delay_for(row.attempts));

        outbox.retry_later(row.id, &err.to_string(), Utc::now() + delay)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sinks::MockConnector;
    use crate::types::Operation;
    use notewise_graph::{EdgeKind, Graph, NodeKind, NodeRef};
    use notewise_storage::{
        Database, ExternalItemRepository, Id, NewOutboxEntry, OutboxRepository, OutboxStatus,
    };
    use std::sync::Arc;

    fn queued(db: &Database, node_id: Id, key: &str) -> Id {
        OutboxRepository::new(db)
            .enqueue(NewOutboxEntry {
                connector_id: "mock".into(),
                node_kind: "action_item".into(),
                node_id,
                operation: Operation::Create.as_str().into(),
                payload: "{}".into(),
                idempotency_key: key.into(),
            })
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn a_successful_delivery_records_an_external_item_and_an_edge() {
        let db = Database::open_in_memory().unwrap();
        let node_id = Id::new();
        queued(&db, node_id, "k1");

        let mut registry = ConnectorRegistry::new();
        registry.register_sink(Arc::new(MockConnector::new("mock")));
        let dispatcher = Dispatcher::new(registry, RetryPolicy::default());

        let report = dispatcher.drain(&db).await.unwrap();
        assert_eq!(report.delivered, 1);
        assert_eq!(report.failed, 0);

        let item = ExternalItemRepository::new(&db)
            .find("mock", "mock-1")
            .unwrap()
            .expect("delivery must record an external item");

        let related = Graph::new(&db)
            .related(NodeRef::new(NodeKind::ActionItem, node_id), 1)
            .unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(
            related[0].node,
            NodeRef::new(NodeKind::ExternalItem, item.id)
        );
        assert_eq!(related[0].via, EdgeKind::SyncedTo);
    }

    #[tokio::test]
    async fn a_delivered_row_is_not_delivered_again() {
        let db = Database::open_in_memory().unwrap();
        queued(&db, Id::new(), "k1");

        let mut registry = ConnectorRegistry::new();
        let mock = Arc::new(MockConnector::new("mock"));
        registry.register_sink(mock.clone());
        let dispatcher = Dispatcher::new(registry, RetryPolicy::default());

        dispatcher.drain(&db).await.unwrap();
        let second = dispatcher.drain(&db).await.unwrap();

        assert_eq!(second.delivered, 0);
        assert_eq!(
            mock.pushed().len(),
            1,
            "a completed row must never be pushed twice"
        );
    }

    #[tokio::test]
    async fn a_second_push_for_a_synced_node_carries_the_existing_reference() {
        let db = Database::open_in_memory().unwrap();
        let node_id = Id::new();
        queued(&db, node_id, "k1");

        let mut registry = ConnectorRegistry::new();
        let mock = Arc::new(MockConnector::new("mock"));
        registry.register_sink(mock.clone());
        let dispatcher = Dispatcher::new(registry, RetryPolicy::default());

        dispatcher.drain(&db).await.unwrap();

        // A genuinely new enqueue for the same node — not a retry.
        queued(&db, node_id, "k2");
        dispatcher.drain(&db).await.unwrap();

        let pushed = mock.pushed();
        assert_eq!(pushed.len(), 2);
        assert!(
            pushed[0].existing.is_none(),
            "the first push has nothing to update"
        );
        assert_eq!(
            pushed[1].existing.as_ref().map(|r| r.external_id.as_str()),
            Some("mock-1"),
            "a sink must be told to update the artifact it already created"
        );
    }

    #[tokio::test]
    async fn a_transient_failure_defers_rather_than_failing() {
        let db = Database::open_in_memory().unwrap();
        queued(&db, Id::new(), "k1");

        let mut registry = ConnectorRegistry::new();
        registry.register_sink(Arc::new(
            MockConnector::new("mock").failing_with(|| ConnectorError::Transient("503".into())),
        ));
        let dispatcher = Dispatcher::new(registry, RetryPolicy::default());

        let report = dispatcher.drain(&db).await.unwrap();
        assert_eq!(report.deferred, 1);
        assert_eq!(report.failed, 0);

        let row = OutboxRepository::new(&db)
            .find_by_key("k1")
            .unwrap()
            .unwrap();
        assert_eq!(row.status, OutboxStatus::Pending);
        assert_eq!(row.attempts, 1);
        assert!(
            row.next_attempt_at > Utc::now(),
            "a retry must be scheduled in the future"
        );
    }

    #[tokio::test]
    async fn an_auth_failure_dead_letters_immediately() {
        let db = Database::open_in_memory().unwrap();
        queued(&db, Id::new(), "k1");

        let mut registry = ConnectorRegistry::new();
        registry.register_sink(Arc::new(MockConnector::new("mock").failing_with(|| {
            ConnectorError::Auth {
                connector: "mock".into(),
            }
        })));
        let dispatcher = Dispatcher::new(registry, RetryPolicy::default());

        let report = dispatcher.drain(&db).await.unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(
            report.deferred, 0,
            "retrying a rejected credential can never succeed"
        );

        let row = OutboxRepository::new(&db)
            .find_by_key("k1")
            .unwrap()
            .unwrap();
        assert_eq!(row.status, OutboxStatus::Failed);
    }

    #[tokio::test]
    async fn exhausted_attempts_dead_letter() {
        let db = Database::open_in_memory().unwrap();
        queued(&db, Id::new(), "k1");

        let mut registry = ConnectorRegistry::new();
        registry.register_sink(Arc::new(
            MockConnector::new("mock").failing_with(|| ConnectorError::Transient("503".into())),
        ));
        let policy = RetryPolicy {
            max_attempts: 2,
            base_delay: Duration::seconds(0),
            ..RetryPolicy::default()
        };
        let dispatcher = Dispatcher::new(registry, policy);

        dispatcher.drain(&db).await.unwrap();
        let second = dispatcher.drain(&db).await.unwrap();

        assert_eq!(second.failed, 1);
        let row = OutboxRepository::new(&db)
            .find_by_key("k1")
            .unwrap()
            .unwrap();
        assert_eq!(row.status, OutboxStatus::Failed);
        assert!(row.last_error.is_some(), "a dead-lettered row must say why");
    }

    #[tokio::test]
    async fn an_unknown_connector_dead_letters_instead_of_looping() {
        let db = Database::open_in_memory().unwrap();
        queued(&db, Id::new(), "k1");

        let dispatcher = Dispatcher::new(ConnectorRegistry::new(), RetryPolicy::default());
        let report = dispatcher.drain(&db).await.unwrap();

        assert_eq!(report.failed, 1);
    }
}
