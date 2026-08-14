//! The outbound delivery queue.
//!
//! Every push to an external system goes through here rather than being called at the site
//! that changed the data. That buys three things a direct call cannot: delivery survives a
//! restart, a retry cannot file a duplicate ticket because `idempotency_key` is unique, and
//! failures stay listable instead of vanishing into a log.

use chrono::{DateTime, Utc};
use rusqlite::Row;

use crate::db::Database;
use crate::error::{Result, StorageError};
use crate::id::Id;
use crate::repositories::decode_enum;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxStatus {
    /// Waiting for a dispatcher.
    Pending,
    /// Claimed by a dispatcher; `leased_until` guards against a second claim.
    InFlight,
    Complete,
    /// Attempts exhausted, or a permanent failure. Retained for inspection.
    Failed,
}

impl OutboxStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            OutboxStatus::Pending => "pending",
            OutboxStatus::InFlight => "in_flight",
            OutboxStatus::Complete => "complete",
            OutboxStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(OutboxStatus::Pending),
            "in_flight" => Some(OutboxStatus::InFlight),
            "complete" => Some(OutboxStatus::Complete),
            "failed" => Some(OutboxStatus::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRecord {
    pub id: Id,
    pub connector_id: String,
    pub node_kind: String,
    pub node_id: Id,
    pub operation: String,
    pub payload: String,
    pub idempotency_key: String,
    pub status: OutboxStatus,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub next_attempt_at: DateTime<Utc>,
    pub leased_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOutboxEntry {
    pub connector_id: String,
    pub node_kind: String,
    pub node_id: Id,
    pub operation: String,
    pub payload: String,
    pub idempotency_key: String,
}

#[derive(Debug)]
pub struct OutboxRepository<'a> {
    db: &'a Database,
}

const COLUMNS: &str = "id, connector_id, node_kind, node_id, operation, payload,
                       idempotency_key, status, attempts, last_error, next_attempt_at,
                       leased_until, created_at";

impl<'a> OutboxRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Queue a delivery, or return the existing row for this idempotency key.
    ///
    /// Returning the existing row rather than erroring is what lets a caller re-enqueue
    /// freely — re-summarizing a meeting should not double-post it.
    pub fn enqueue(&self, new: NewOutboxEntry) -> Result<OutboxRecord> {
        if let Some(existing) = self.find_by_key(&new.idempotency_key)? {
            return Ok(existing);
        }

        let now = Utc::now();
        self.db.conn().execute(
            "INSERT INTO connector_outbox
                (id, connector_id, node_kind, node_id, operation, payload, idempotency_key,
                 status, attempts, next_attempt_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?9)",
            rusqlite::params![
                Id::new(),
                new.connector_id,
                new.node_kind,
                new.node_id,
                new.operation,
                new.payload,
                new.idempotency_key,
                OutboxStatus::Pending.as_str(),
                now
            ],
        )?;

        self.find_by_key(&new.idempotency_key)?
            .ok_or_else(|| StorageError::not_found("outbox_entry", new.idempotency_key))
    }

    pub fn find_by_key(&self, key: &str) -> Result<Option<OutboxRecord>> {
        let conn = self.db.conn();
        let sql = format!("SELECT {COLUMNS} FROM connector_outbox WHERE idempotency_key = ?1");
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params![key])?;

        match rows.next()? {
            Some(row) => Ok(Some(read_row(row)?)),
            None => Ok(None),
        }
    }
}

fn read_row(row: &Row<'_>) -> Result<OutboxRecord> {
    let raw_status: String = row.get(7)?;
    Ok(OutboxRecord {
        id: row.get(0)?,
        connector_id: row.get(1)?,
        node_kind: row.get(2)?,
        node_id: row.get(3)?,
        operation: row.get(4)?,
        payload: row.get(5)?,
        idempotency_key: row.get(6)?,
        status: decode_enum("status", &raw_status, OutboxStatus::parse)?,
        attempts: row.get(8)?,
        last_error: row.get(9)?,
        next_attempt_at: row.get(10)?,
        leased_until: row.get(11)?,
        created_at: row.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory database")
    }

    fn entry(key: &str) -> NewOutboxEntry {
        NewOutboxEntry {
            connector_id: "vault".into(),
            node_kind: "meeting".into(),
            node_id: Id::new(),
            operation: "create".into(),
            payload: "{}".into(),
            idempotency_key: key.into(),
        }
    }

    #[test]
    fn enqueue_starts_pending_and_ready() {
        let db = db();
        let repo = OutboxRepository::new(&db);

        let row = repo.enqueue(entry("k1")).unwrap();
        assert_eq!(row.status, OutboxStatus::Pending);
        assert_eq!(row.attempts, 0);
        assert!(row.leased_until.is_none());
    }

    #[test]
    fn enqueueing_the_same_key_twice_yields_one_row() {
        let db = db();
        let repo = OutboxRepository::new(&db);

        let first = repo.enqueue(entry("same")).unwrap();
        let second = repo.enqueue(entry("same")).unwrap();

        assert_eq!(
            first.id, second.id,
            "duplicate enqueue must not create a second delivery"
        );

        let count: u32 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM connector_outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn distinct_keys_are_distinct_rows() {
        let db = db();
        let repo = OutboxRepository::new(&db);

        let a = repo.enqueue(entry("k1")).unwrap();
        let b = repo.enqueue(entry("k2")).unwrap();
        assert_ne!(a.id, b.id);
    }
}
