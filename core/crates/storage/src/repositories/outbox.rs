//! The outbound delivery queue.
//!
//! Every push to an external system goes through here rather than being called at the site
//! that changed the data. That buys three things a direct call cannot: delivery survives a
//! restart, a retry cannot file a duplicate ticket because `idempotency_key` is unique, and
//! failures stay listable instead of vanishing into a log.

use chrono::{DateTime, Duration, Utc};
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

    /// Claim up to `limit` rows that are due, leasing them for `lease`.
    ///
    /// The lease exists so a dispatcher that dies mid-delivery does not strand its rows
    /// forever. Rows whose lease has expired are reclaimable.
    pub fn claim_ready(&self, limit: u32, lease: Duration) -> Result<Vec<OutboxRecord>> {
        let now = Utc::now();
        let leased_until = now + lease;

        let conn = self.db.conn();
        let sql = format!(
            "SELECT {COLUMNS} FROM connector_outbox
             WHERE status IN ('pending', 'in_flight')
               AND next_attempt_at <= ?1
               AND (leased_until IS NULL OR leased_until <= ?1)
             ORDER BY next_attempt_at
             LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        let due: Vec<OutboxRecord> = stmt
            .query_map(rusqlite::params![now, limit], |row| Ok(read_row(row)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        drop(stmt);

        let mut claimed = Vec::with_capacity(due.len());
        for row in due {
            self.db.conn().execute(
                "UPDATE connector_outbox SET status = ?2, leased_until = ?3 WHERE id = ?1",
                rusqlite::params![row.id, OutboxStatus::InFlight.as_str(), leased_until],
            )?;
            claimed.push(OutboxRecord {
                status: OutboxStatus::InFlight,
                leased_until: Some(leased_until),
                ..row
            });
        }

        Ok(claimed)
    }

    pub fn complete(&self, id: Id) -> Result<()> {
        self.db.conn().execute(
            "UPDATE connector_outbox
             SET status = ?2, leased_until = NULL, last_error = NULL
             WHERE id = ?1",
            rusqlite::params![id, OutboxStatus::Complete.as_str()],
        )?;
        Ok(())
    }

    /// Return the row to the queue, due at `next_attempt_at`.
    pub fn retry_later(&self, id: Id, error: &str, next_attempt_at: DateTime<Utc>) -> Result<()> {
        self.db.conn().execute(
            "UPDATE connector_outbox
             SET status = ?2, attempts = attempts + 1, last_error = ?3,
                 next_attempt_at = ?4, leased_until = NULL
             WHERE id = ?1",
            rusqlite::params![id, OutboxStatus::Pending.as_str(), error, next_attempt_at],
        )?;
        Ok(())
    }

    /// Give up on this delivery, but keep it visible.
    ///
    /// Silently dropping a failed push is worse than never having attempted it.
    pub fn dead_letter(&self, id: Id, error: &str) -> Result<()> {
        self.db.conn().execute(
            "UPDATE connector_outbox
             SET status = ?2, attempts = attempts + 1, last_error = ?3, leased_until = NULL
             WHERE id = ?1",
            rusqlite::params![id, OutboxStatus::Failed.as_str(), error],
        )?;
        Ok(())
    }

    pub fn list_failed(&self, limit: u32) -> Result<Vec<OutboxRecord>> {
        let conn = self.db.conn();
        let sql = format!(
            "SELECT {COLUMNS} FROM connector_outbox
             WHERE status = 'failed' ORDER BY created_at DESC LIMIT ?1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<Result<OutboxRecord>> = stmt
            .query_map(rusqlite::params![limit], |row| Ok(read_row(row)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter().collect()
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

    use chrono::Duration;

    #[test]
    fn claim_returns_pending_rows_and_leases_them() {
        let db = db();
        let repo = OutboxRepository::new(&db);
        repo.enqueue(entry("k1")).unwrap();

        let claimed = repo.claim_ready(10, Duration::minutes(5)).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].status, OutboxStatus::InFlight);
        assert!(claimed[0].leased_until.is_some());
    }

    #[test]
    fn a_leased_row_is_not_claimed_twice() {
        let db = db();
        let repo = OutboxRepository::new(&db);
        repo.enqueue(entry("k1")).unwrap();

        assert_eq!(repo.claim_ready(10, Duration::minutes(5)).unwrap().len(), 1);
        assert!(
            repo.claim_ready(10, Duration::minutes(5))
                .unwrap()
                .is_empty(),
            "a live lease must block a second dispatcher"
        );
    }

    #[test]
    fn an_expired_lease_is_reclaimable() {
        let db = db();
        let repo = OutboxRepository::new(&db);
        repo.enqueue(entry("k1")).unwrap();

        repo.claim_ready(10, Duration::seconds(-1)).unwrap();
        assert_eq!(
            repo.claim_ready(10, Duration::minutes(5)).unwrap().len(),
            1,
            "a dispatcher that died must not strand the row"
        );
    }

    #[test]
    fn complete_marks_the_row_done() {
        let db = db();
        let repo = OutboxRepository::new(&db);
        let row = repo.enqueue(entry("k1")).unwrap();

        repo.complete(row.id).unwrap();

        let after = repo.find_by_key("k1").unwrap().expect("row exists");
        assert_eq!(after.status, OutboxStatus::Complete);
        assert!(repo
            .claim_ready(10, Duration::minutes(5))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn retry_increments_attempts_and_defers() {
        let db = db();
        let repo = OutboxRepository::new(&db);
        let row = repo.enqueue(entry("k1")).unwrap();
        repo.claim_ready(10, Duration::minutes(5)).unwrap();

        repo.retry_later(row.id, "503 upstream", Utc::now() + Duration::minutes(2))
            .unwrap();

        let after = repo.find_by_key("k1").unwrap().expect("row exists");
        assert_eq!(after.status, OutboxStatus::Pending);
        assert_eq!(after.attempts, 1);
        assert_eq!(after.last_error.as_deref(), Some("503 upstream"));
        assert!(
            after.leased_until.is_none(),
            "a deferred row must not stay leased"
        );
        assert!(
            repo.claim_ready(10, Duration::minutes(5))
                .unwrap()
                .is_empty(),
            "a row deferred into the future must not be claimed now"
        );
    }

    #[test]
    fn dead_letter_keeps_the_row_listable() {
        let db = db();
        let repo = OutboxRepository::new(&db);
        let row = repo.enqueue(entry("k1")).unwrap();

        repo.dead_letter(row.id, "401 unauthorized").unwrap();

        let failed = repo.list_failed(10).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].status, OutboxStatus::Failed);
        assert_eq!(failed[0].last_error.as_deref(), Some("401 unauthorized"));
        assert!(repo
            .claim_ready(10, Duration::minutes(5))
            .unwrap()
            .is_empty());
    }
}
