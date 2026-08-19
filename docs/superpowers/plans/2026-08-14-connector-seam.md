# Connector Seam Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the seam every external service hides behind — direction-split connector traits with an idempotent delivery outbox — plus two local sinks that prove it works without an OAuth app.

**Architecture:** A new MIT crate `core/crates/connectors` depends on `storage` and `graph` and on no surface. `SourceConnector` pulls, `SinkConnector` pushes; capability is which trait you implement, not a runtime flag. Outbound work is enqueued to a `connector_outbox` table with an idempotency key, drained by a `Dispatcher` that classifies failures into retry, backoff, or dead-letter, and records results as `ExternalItem` nodes joined by `SyncedTo` edges.

**Tech Stack:** Rust 2021, `rusqlite` (bundled), `axum`, `reqwest`, `async-trait`, `thiserror`, `tokio`, `sha2`, `hmac`, `keyring`, `tempfile`.

**Spec:** `docs/superpowers/specs/2026-08-14-connector-architecture-design.md`

> **Committing while other sessions share this checkout.** Every commit step below writes
> `git commit -m "..." -- <paths>`, with the paths repeated after `--`. That is not
> redundancy. `git add <paths>` scopes only the *index*; a bare `git commit` then commits
> the entire index, including anything another session happened to have staged at that
> moment. Scoping the `add` is not enough — the index is shared state even when the paths
> are not. This has already happened once on this branch in the other direction, sweeping
> four half-written files of ours into an unrelated commit.

---

## File Structure

**`core/crates/storage/`** — owns SQL, per rule 3.

| File | Responsibility |
|---|---|
| `src/migrations.rs` | Append migration v5 |
| `src/repositories/external_item.rs` | `ExternalItem`, `NewExternalItem`, `ExternalItemRepository` |
| `src/repositories/connector_account.rs` | `ConnectorAccount`, `ConnectorAccountRepository` |
| `src/repositories/outbox.rs` | `OutboxRecord`, `OutboxStatus`, `NewOutboxEntry`, `OutboxRepository` |
| `src/repositories/mod.rs` | Register and re-export the three |

**`core/crates/graph/`**

| File | Responsibility |
|---|---|
| `src/kinds.rs` | `NodeKind::ExternalItem`, `EdgeKind::SyncedTo` |

**`core/crates/connectors/`** — new crate.

| File | Responsibility |
|---|---|
| `src/lib.rs` | Crate docs, module wiring, public re-exports |
| `src/error.rs` | `ConnectorError`, `Result`, retry classification |
| `src/types.rs` | `Operation`, `Outbound`, `Inbound`, `ExternalRef`, `Cursor`, `PullBatch`, `Health` |
| `src/connector.rs` | `Connector`, `SourceConnector`, `SinkConnector` traits |
| `src/credentials.rs` | `Secret`, `CredentialStore`, `MemoryStore` |
| `src/keychain.rs` | `KeychainStore` |
| `src/registry.rs` | `ConnectorRegistry` |
| `src/config.rs` | Build a registry from persisted accounts + credentials |
| `src/dispatcher.rs` | Outbox drain loop, backoff, graph write-back |
| `src/sinks/mock.rs` | `MockConnector` |
| `src/sinks/vault.rs` | `VaultSink` |
| `src/sinks/webhook.rs` | `WebhookSink` |

**`core/crates/api-server/`**

| File | Responsibility |
|---|---|
| `src/connectors.rs` | List, connect, disconnect, and failed-delivery handlers |
| `src/routes.rs` | Route registration |
| `src/state.rs` | Hold the `ConnectorRegistry`, swappable at runtime |

---

## Task 1: Schema v5

**Files:**
- Modify: `core/crates/storage/src/migrations.rs`
- Modify: `Cargo.toml` (workspace dependencies)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module at the bottom of `core/crates/storage/src/migrations.rs`:

```rust
    #[test]
    fn v5_creates_connector_tables() {
        let mut conn = fresh();
        migrate(&mut conn).unwrap();

        for table in ["external_items", "connector_accounts", "connector_outbox"] {
            let count: u32 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing table {table}");
        }
    }

    #[test]
    fn outbox_idempotency_key_is_unique() {
        let mut conn = fresh();
        migrate(&mut conn).unwrap();

        let insert = "INSERT INTO connector_outbox
            (id, connector_id, node_kind, node_id, operation, payload, idempotency_key,
             status, attempts, next_attempt_at, created_at)
            VALUES (?1, 'vault', 'meeting', 'n1', 'create', '{}', 'dupe',
                    'pending', 0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')";

        conn.execute(insert, ["a"]).unwrap();
        assert!(
            conn.execute(insert, ["b"]).is_err(),
            "a second row with the same idempotency_key must be rejected"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-storage v5_creates_connector_tables`
Expected: FAIL — `missing table external_items`

- [ ] **Step 3: Append migration v5**

Append to the `MIGRATIONS` array in `core/crates/storage/src/migrations.rs`, after the v3 entry. Migrations are append-only — do not edit v1–v3. `SUPPORTED_VERSION` derives from `MIGRATIONS.len()`, so it updates itself.

```rust
    // v5 — the connector seam: external artifact records, per-connector account state, and
    // the outbound delivery queue. No tokens live here; credentials go to the OS keychain.
    r#"
    CREATE TABLE external_items (
        id              TEXT PRIMARY KEY NOT NULL,
        connector_id    TEXT NOT NULL,
        external_id     TEXT NOT NULL,
        url             TEXT,
        title           TEXT,
        remote_version  TEXT,
        last_synced_at  TEXT NOT NULL,
        created_at      TEXT NOT NULL
    );
    CREATE UNIQUE INDEX idx_external_items_identity
        ON external_items(connector_id, external_id);

    CREATE TABLE connector_accounts (
        connector_id   TEXT PRIMARY KEY NOT NULL,
        account_label  TEXT,
        scopes         TEXT NOT NULL,
        status         TEXT NOT NULL,
        connected_at   TEXT NOT NULL,
        cursor         TEXT
    );

    CREATE TABLE connector_outbox (
        id               TEXT PRIMARY KEY NOT NULL,
        connector_id     TEXT NOT NULL,
        node_kind        TEXT NOT NULL,
        node_id          TEXT NOT NULL,
        operation        TEXT NOT NULL,
        payload          TEXT NOT NULL,
        idempotency_key  TEXT NOT NULL,
        status           TEXT NOT NULL,
        attempts         INTEGER NOT NULL DEFAULT 0,
        last_error       TEXT,
        next_attempt_at  TEXT NOT NULL,
        leased_until     TEXT,
        created_at       TEXT NOT NULL
    );
    CREATE UNIQUE INDEX idx_outbox_idempotency ON connector_outbox(idempotency_key);
    CREATE INDEX idx_outbox_ready ON connector_outbox(status, next_attempt_at);
    "#,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p notewise-storage migrations`
Expected: PASS, including the pre-existing `migrate_is_idempotent` and version tests.

- [ ] **Step 5: Add shared dependencies**

In the root `Cargo.toml`, add to `[workspace.dependencies]` after the `clap` line:

```toml
notewise-connectors = { path = "core/crates/connectors", version = "0.1.0" }
sha2 = "0.10"
hmac = "0.12"
hex = "0.4"
keyring = "3"
tempfile = "3"
```

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml core/crates/storage/src/migrations.rs
git commit -m "feat(storage): add schema v5 for the connector seam"
```

---

## Task 2: ExternalItemRepository

**Files:**
- Create: `core/crates/storage/src/repositories/external_item.rs`
- Modify: `core/crates/storage/src/repositories/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `core/crates/storage/src/repositories/external_item.rs` with the test module only for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory database")
    }

    fn sample() -> NewExternalItem {
        NewExternalItem {
            connector_id: "linear".into(),
            external_id: "ENG-412".into(),
            url: Some("https://linear.app/x/issue/ENG-412".into()),
            title: Some("Fix login".into()),
            remote_version: Some("1".into()),
        }
    }

    #[test]
    fn upsert_creates_then_updates_in_place() {
        let db = db();
        let repo = ExternalItemRepository::new(&db);

        let first = repo.upsert(sample()).unwrap();
        assert_eq!(first.external_id, "ENG-412");

        let second = repo
            .upsert(NewExternalItem {
                title: Some("Fix login redirect".into()),
                remote_version: Some("2".into()),
                ..sample()
            })
            .unwrap();

        assert_eq!(second.id, first.id, "upsert must not create a second row");
        assert_eq!(second.title.as_deref(), Some("Fix login redirect"));
        assert_eq!(second.remote_version.as_deref(), Some("2"));
    }

    #[test]
    fn same_external_id_on_a_different_connector_is_a_different_item() {
        let db = db();
        let repo = ExternalItemRepository::new(&db);

        let linear = repo.upsert(sample()).unwrap();
        let jira = repo
            .upsert(NewExternalItem {
                connector_id: "jira".into(),
                ..sample()
            })
            .unwrap();

        assert_ne!(linear.id, jira.id);
    }

    #[test]
    fn find_returns_none_for_unknown_identity() {
        let db = db();
        let repo = ExternalItemRepository::new(&db);
        assert!(repo.find("linear", "ENG-999").unwrap().is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-storage external_item`
Expected: FAIL to compile — `cannot find type ExternalItemRepository`

- [ ] **Step 3: Write the implementation**

Prepend to `core/crates/storage/src/repositories/external_item.rs`:

```rust
//! Records of artifacts that live in another system.
//!
//! An external item is owned by nothing in Notewise — it is a record of a thing elsewhere.
//! So it carries no owning foreign key; association to a meeting or action item is a
//! `synced_to` edge in the `graph` crate. Identity is `(connector_id, external_id)`, which
//! is what makes re-pushing update a ticket rather than file a second one.

use chrono::{DateTime, Utc};
use rusqlite::Row;

use crate::db::Database;
use crate::error::Result;
use crate::id::Id;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalItem {
    pub id: Id,
    pub connector_id: String,
    pub external_id: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub remote_version: Option<String>,
    pub last_synced_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewExternalItem {
    pub connector_id: String,
    pub external_id: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub remote_version: Option<String>,
}

#[derive(Debug)]
pub struct ExternalItemRepository<'a> {
    db: &'a Database,
}

impl<'a> ExternalItemRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Insert, or update the existing row for this `(connector_id, external_id)`.
    pub fn upsert(&self, new: NewExternalItem) -> Result<ExternalItem> {
        let now = Utc::now();
        let id = self
            .find(&new.connector_id, &new.external_id)?
            .map(|existing| existing.id)
            .unwrap_or_else(Id::new);

        self.db.conn().execute(
            "INSERT INTO external_items
                (id, connector_id, external_id, url, title, remote_version,
                 last_synced_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(connector_id, external_id) DO UPDATE SET
                url            = excluded.url,
                title          = excluded.title,
                remote_version = excluded.remote_version,
                last_synced_at = excluded.last_synced_at",
            rusqlite::params![
                id,
                new.connector_id,
                new.external_id,
                new.url,
                new.title,
                new.remote_version,
                now
            ],
        )?;

        self.find(&new.connector_id, &new.external_id)?
            .ok_or_else(|| crate::error::StorageError::not_found("external_item", new.external_id))
    }

    pub fn find(&self, connector_id: &str, external_id: &str) -> Result<Option<ExternalItem>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, connector_id, external_id, url, title, remote_version,
                    last_synced_at, created_at
             FROM external_items
             WHERE connector_id = ?1 AND external_id = ?2",
        )?;

        let mut rows = stmt.query(rusqlite::params![connector_id, external_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(read_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn get(&self, id: Id) -> Result<ExternalItem> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, connector_id, external_id, url, title, remote_version,
                    last_synced_at, created_at
             FROM external_items WHERE id = ?1",
        )?;

        let mut rows = stmt.query(rusqlite::params![id])?;
        match rows.next()? {
            Some(row) => read_row(row),
            None => Err(crate::error::StorageError::not_found("external_item", id)),
        }
    }
}

fn read_row(row: &Row<'_>) -> Result<ExternalItem> {
    Ok(ExternalItem {
        id: row.get(0)?,
        connector_id: row.get(1)?,
        external_id: row.get(2)?,
        url: row.get(3)?,
        title: row.get(4)?,
        remote_version: row.get(5)?,
        last_synced_at: row.get(6)?,
        created_at: row.get(7)?,
    })
}
```

- [ ] **Step 4: Register the module**

In `core/crates/storage/src/repositories/mod.rs`, add `mod external_item;` to the module list (alphabetical, after `mod edge;`) and this re-export after the `edge` one:

```rust
pub use external_item::{ExternalItem, ExternalItemRepository, NewExternalItem};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p notewise-storage external_item`
Expected: PASS — 3 tests.

- [ ] **Step 6: Commit**

```bash
git add core/crates/storage/src/repositories/
git commit -m "feat(storage): add the external item repository"
```

---

## Task 3: ConnectorAccountRepository

**Files:**
- Create: `core/crates/storage/src/repositories/connector_account.rs`
- Modify: `core/crates/storage/src/repositories/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `core/crates/storage/src/repositories/connector_account.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory database")
    }

    #[test]
    fn connect_then_read_back() {
        let db = db();
        let repo = ConnectorAccountRepository::new(&db);

        repo.connect("vault", Some("~/Notes"), &["write".into()])
            .unwrap();

        let account = repo.get("vault").unwrap().expect("just connected");
        assert_eq!(account.account_label.as_deref(), Some("~/Notes"));
        assert_eq!(account.status, AccountStatus::Connected);
        assert!(account.cursor.is_none());
    }

    #[test]
    fn needs_reauth_survives_a_read() {
        let db = db();
        let repo = ConnectorAccountRepository::new(&db);

        repo.connect("google_calendar", Some("a@b.com"), &["ro".into()])
            .unwrap();
        repo.set_status("google_calendar", AccountStatus::NeedsReauth)
            .unwrap();

        let account = repo.get("google_calendar").unwrap().expect("connected");
        assert_eq!(account.status, AccountStatus::NeedsReauth);
    }

    #[test]
    fn cursor_advances_independently_of_status() {
        let db = db();
        let repo = ConnectorAccountRepository::new(&db);

        repo.connect("google_calendar", None, &[]).unwrap();
        repo.set_cursor("google_calendar", Some("page-2")).unwrap();

        let account = repo.get("google_calendar").unwrap().expect("connected");
        assert_eq!(account.cursor.as_deref(), Some("page-2"));
        assert_eq!(account.status, AccountStatus::Connected);
    }

    #[test]
    fn disconnect_removes_the_account() {
        let db = db();
        let repo = ConnectorAccountRepository::new(&db);

        repo.connect("vault", None, &[]).unwrap();
        repo.disconnect("vault").unwrap();

        assert!(repo.get("vault").unwrap().is_none());
    }

    #[test]
    fn unknown_status_in_the_database_is_an_error_not_a_panic() {
        let db = db();
        db.conn()
            .execute(
                "INSERT INTO connector_accounts
                    (connector_id, account_label, scopes, status, connected_at)
                 VALUES ('weird', NULL, '', 'ascended', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();

        let repo = ConnectorAccountRepository::new(&db);
        assert!(repo.get("weird").is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-storage connector_account`
Expected: FAIL to compile — `cannot find type ConnectorAccountRepository`

- [ ] **Step 3: Write the implementation**

Prepend to `core/crates/storage/src/repositories/connector_account.rs`:

```rust
//! Per-connector account state.
//!
//! Deliberately holds **no tokens**. Credentials live in the OS keychain, addressed by
//! `connector_id`; a refresh token in this file would travel with any copy of the database,
//! including one attached to a bug report.

use chrono::{DateTime, Utc};

use crate::db::Database;
use crate::error::Result;
use crate::repositories::decode_enum;

/// Whether a connector can currently be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    Connected,
    /// Credentials were rejected. Retrying cannot fix this; the user must reconnect.
    NeedsReauth,
    /// Connected but paused by the user.
    Disabled,
}

impl AccountStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AccountStatus::Connected => "connected",
            AccountStatus::NeedsReauth => "needs_reauth",
            AccountStatus::Disabled => "disabled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "connected" => Some(AccountStatus::Connected),
            "needs_reauth" => Some(AccountStatus::NeedsReauth),
            "disabled" => Some(AccountStatus::Disabled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorAccount {
    pub connector_id: String,
    pub account_label: Option<String>,
    pub scopes: Vec<String>,
    pub status: AccountStatus,
    pub connected_at: DateTime<Utc>,
    pub cursor: Option<String>,
}

#[derive(Debug)]
pub struct ConnectorAccountRepository<'a> {
    db: &'a Database,
}

impl<'a> ConnectorAccountRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Record a connected account, replacing any previous one for this connector.
    pub fn connect(
        &self,
        connector_id: &str,
        account_label: Option<&str>,
        scopes: &[String],
    ) -> Result<()> {
        self.db.conn().execute(
            "INSERT INTO connector_accounts
                (connector_id, account_label, scopes, status, connected_at, cursor)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)
             ON CONFLICT(connector_id) DO UPDATE SET
                account_label = excluded.account_label,
                scopes        = excluded.scopes,
                status        = excluded.status,
                connected_at  = excluded.connected_at",
            rusqlite::params![
                connector_id,
                account_label,
                scopes.join(" "),
                AccountStatus::Connected.as_str(),
                Utc::now()
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, connector_id: &str) -> Result<Option<ConnectorAccount>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT connector_id, account_label, scopes, status, connected_at, cursor
             FROM connector_accounts WHERE connector_id = ?1",
        )?;

        let mut rows = stmt.query(rusqlite::params![connector_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };

        let raw_status: String = row.get(3)?;
        let raw_scopes: String = row.get(2)?;

        Ok(Some(ConnectorAccount {
            connector_id: row.get(0)?,
            account_label: row.get(1)?,
            scopes: raw_scopes
                .split_whitespace()
                .map(str::to_string)
                .collect(),
            status: decode_enum("status", &raw_status, AccountStatus::parse)?,
            connected_at: row.get(4)?,
            cursor: row.get(5)?,
        }))
    }

    pub fn list(&self) -> Result<Vec<ConnectorAccount>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare("SELECT connector_id FROM connector_accounts ORDER BY connector_id")?;
        let ids: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);

        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(account) = self.get(&id)? {
                out.push(account);
            }
        }
        Ok(out)
    }

    pub fn set_status(&self, connector_id: &str, status: AccountStatus) -> Result<()> {
        self.db.conn().execute(
            "UPDATE connector_accounts SET status = ?2 WHERE connector_id = ?1",
            rusqlite::params![connector_id, status.as_str()],
        )?;
        Ok(())
    }

    pub fn set_cursor(&self, connector_id: &str, cursor: Option<&str>) -> Result<()> {
        self.db.conn().execute(
            "UPDATE connector_accounts SET cursor = ?2 WHERE connector_id = ?1",
            rusqlite::params![connector_id, cursor],
        )?;
        Ok(())
    }

    /// Remove the account. Removing an absent account succeeds.
    pub fn disconnect(&self, connector_id: &str) -> Result<()> {
        self.db.conn().execute(
            "DELETE FROM connector_accounts WHERE connector_id = ?1",
            rusqlite::params![connector_id],
        )?;
        Ok(())
    }
}
```

- [ ] **Step 4: Register the module**

In `core/crates/storage/src/repositories/mod.rs`, add `mod connector_account;` and:

```rust
pub use connector_account::{AccountStatus, ConnectorAccount, ConnectorAccountRepository};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p notewise-storage connector_account`
Expected: PASS — 5 tests.

- [ ] **Step 6: Commit**

```bash
git add core/crates/storage/src/repositories/
git commit -m "feat(storage): add the connector account repository"
```

---

## Task 4: OutboxRepository — enqueue and idempotency

**Files:**
- Create: `core/crates/storage/src/repositories/outbox.rs`
- Modify: `core/crates/storage/src/repositories/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `core/crates/storage/src/repositories/outbox.rs` with the test module:

```rust
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

        assert_eq!(first.id, second.id, "duplicate enqueue must not create a second delivery");

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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-storage outbox`
Expected: FAIL to compile — `cannot find type OutboxRepository`

- [ ] **Step 3: Write the implementation**

Prepend to `core/crates/storage/src/repositories/outbox.rs`:

```rust
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
```

- [ ] **Step 4: Register the module**

In `core/crates/storage/src/repositories/mod.rs`, add `mod outbox;` and:

```rust
pub use outbox::{NewOutboxEntry, OutboxRecord, OutboxRepository, OutboxStatus};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p notewise-storage outbox`
Expected: PASS — 3 tests.

- [ ] **Step 6: Commit**

```bash
git add core/crates/storage/src/repositories/
git commit -m "feat(storage): add outbox enqueue with idempotency"
```

---

## Task 5: OutboxRepository — claim, complete, fail

**Files:**
- Modify: `core/crates/storage/src/repositories/outbox.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `core/crates/storage/src/repositories/outbox.rs`:

```rust
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
            repo.claim_ready(10, Duration::minutes(5)).unwrap().is_empty(),
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
        assert!(repo.claim_ready(10, Duration::minutes(5)).unwrap().is_empty());
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
        assert!(after.leased_until.is_none(), "a deferred row must not stay leased");
        assert!(
            repo.claim_ready(10, Duration::minutes(5)).unwrap().is_empty(),
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
        assert!(repo.claim_ready(10, Duration::minutes(5)).unwrap().is_empty());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-storage outbox`
Expected: FAIL to compile — `no method named claim_ready`

- [ ] **Step 3: Write the implementation**

Widen the chrono import at the top of `core/crates/storage/src/repositories/outbox.rs` to
`use chrono::{DateTime, Duration, Utc};`, then add these methods inside
`impl<'a> OutboxRepository<'a>`:

```rust
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
            .query_map(rusqlite::params![now, limit], |row| {
                Ok(read_row(row).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
                }))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        drop(stmt);
        drop(conn);

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
    pub fn retry_later(
        &self,
        id: Id,
        error: &str,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<()> {
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p notewise-storage outbox`
Expected: PASS — 9 tests.

- [ ] **Step 5: Commit**

```bash
git add core/crates/storage/src/repositories/outbox.rs
git commit -m "feat(storage): add outbox claim, retry, and dead-letter"
```

---

## Task 6: Graph kinds for external items

**Files:**
- Modify: `core/crates/graph/src/kinds.rs`

- [ ] **Step 1: Write the failing test**

In the `tests` module of `core/crates/graph/src/kinds.rs`, change the two assertions in `all_lists_are_exhaustive` and add a new test:

```rust
    #[test]
    fn all_lists_are_exhaustive() {
        // A missing entry in ALL silently breaks `parse`, so guard the count.
        assert_eq!(NodeKind::ALL.len(), 12);
        assert_eq!(EdgeKind::ALL.len(), 9);
    }

    #[test]
    fn external_items_are_reachable_kinds() {
        assert_eq!(NodeKind::parse("external_item"), Some(NodeKind::ExternalItem));
        assert_eq!(EdgeKind::parse("synced_to"), Some(EdgeKind::SyncedTo));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-graph kinds`
Expected: FAIL — `no variant named ExternalItem found for enum NodeKind`

- [ ] **Step 3: Add the variants**

In `core/crates/graph/src/kinds.rs`, make four edits.

Add to the `NodeKind` enum after `Notification`:

```rust
    /// An artifact that lives in another system — a Linear issue, a calendar event, a file
    /// in a user's vault. Notewise records it but does not own it.
    ExternalItem,
```

Add `NodeKind::ExternalItem,` to the end of `NodeKind::ALL`, and this arm to `NodeKind::as_str`:

```rust
            NodeKind::ExternalItem => "external_item",
```

Add to the `EdgeKind` enum after `NotifiesAbout`:

```rust
    /// This node is mirrored in an external system, e.g. action item → Linear issue.
    SyncedTo,
```

Add `EdgeKind::SyncedTo,` to the end of `EdgeKind::ALL`, and this arm to `EdgeKind::as_str`:

```rust
            EdgeKind::SyncedTo => "synced_to",
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p notewise-graph`
Expected: PASS — including the existing round-trip and uniqueness tests, which now cover the new variants automatically.

- [ ] **Step 5: Commit**

```bash
git add core/crates/graph/src/kinds.rs
git commit -m "feat(graph): add external_item nodes and synced_to edges"
```

---

## Task 7: The connectors crate — errors and types

**Files:**
- Create: `core/crates/connectors/Cargo.toml`
- Create: `core/crates/connectors/src/lib.rs`
- Create: `core/crates/connectors/src/error.rs`
- Create: `core/crates/connectors/src/types.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Create the crate manifest**

Create `core/crates/connectors/Cargo.toml`:

```toml
[package]
name = "notewise-connectors"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true
description = "The seam every external service in Notewise hides behind."

[dependencies]
notewise-storage.workspace = true
notewise-graph.workspace = true
async-trait.workspace = true
chrono.workspace = true
serde = { workspace = true }
serde_json.workspace = true
thiserror.workspace = true
tracing.workspace = true
reqwest.workspace = true
sha2.workspace = true
hmac.workspace = true
hex.workspace = true
keyring.workspace = true

[dev-dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "time"] }
tempfile.workspace = true
axum.workspace = true
```

Add `"core/crates/connectors",` to `members` in the root `Cargo.toml`, after `"core/crates/graph",`.

- [ ] **Step 2: Write the failing test**

Create `core/crates/connectors/src/error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn auth_failures_are_not_retryable() {
        let err = ConnectorError::Auth { connector: "google_calendar".into() };
        assert!(!err.is_retryable(), "retrying a rejected credential burns quota forever");
    }

    #[test]
    fn transient_and_rate_limit_failures_are_retryable() {
        assert!(ConnectorError::Transient("503".into()).is_retryable());
        assert!(ConnectorError::RateLimited { retry_after: Duration::from_secs(30) }.is_retryable());
    }

    #[test]
    fn permanent_failures_are_not_retryable() {
        assert!(!ConnectorError::Permanent("422 malformed".into()).is_retryable());
    }

    #[test]
    fn rate_limits_report_the_vendors_own_delay() {
        let err = ConnectorError::RateLimited { retry_after: Duration::from_secs(90) };
        assert_eq!(err.retry_after(), Some(Duration::from_secs(90)));
        assert_eq!(ConnectorError::Transient("503".into()).retry_after(), None);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p notewise-connectors`
Expected: FAIL — the crate has no `lib.rs` yet.

- [ ] **Step 4: Write the error type**

Prepend to `core/crates/connectors/src/error.rs`:

```rust
use std::time::Duration;

use notewise_graph::GraphError;
use notewise_storage::StorageError;
use thiserror::Error;

/// Errors from a connector.
///
/// Variants exist to drive retry policy, not merely to describe what went wrong. The
/// dispatcher branches on these, so a new variant is a scheduling decision.
#[derive(Debug, Error)]
pub enum ConnectorError {
    #[error("{connector} rejected our credentials; reconnect the account")]
    Auth { connector: String },

    #[error("rate limited; retry after {retry_after:?}")]
    RateLimited { retry_after: Duration },

    #[error("temporary failure: {0}")]
    Transient(String),

    #[error("permanent failure: {0}")]
    Permanent(String),

    #[error("connector is not configured: {0}")]
    NotConfigured(String),

    #[error("no connector registered with id '{0}'")]
    UnknownConnector(String),

    #[error("credential store error: {0}")]
    Credential(String),

    #[error(transparent)]
    Storage(#[from] StorageError),

    #[error(transparent)]
    Graph(#[from] GraphError),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl ConnectorError {
    /// Whether the dispatcher should schedule another attempt.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ConnectorError::Transient(_) | ConnectorError::RateLimited { .. }
        )
    }

    /// A delay the remote service asked us to honour, if it named one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            ConnectorError::RateLimited { retry_after } => Some(*retry_after),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, ConnectorError>;
```

- [ ] **Step 5: Write the shared types**

Create `core/crates/connectors/src/types.rs`:

```rust
//! Types crossing the connector boundary.

use chrono::{DateTime, Utc};
use notewise_storage::Id;
use serde::{Deserialize, Serialize};

/// What a push is asking the remote system to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Create,
    Update,
    Delete,
}

impl Operation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Operation::Create => "create",
            Operation::Update => "update",
            Operation::Delete => "delete",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "create" => Some(Operation::Create),
            "update" => Some(Operation::Update),
            "delete" => Some(Operation::Delete),
            _ => None,
        }
    }
}

/// A unit of work handed to a [`SinkConnector`](crate::SinkConnector).
#[derive(Debug, Clone, PartialEq)]
pub struct Outbound {
    pub node_kind: String,
    pub node_id: Id,
    pub operation: Operation,
    pub payload: serde_json::Value,
    /// Set when this node has been pushed before, so the sink updates rather than creates.
    pub existing: Option<ExternalRef>,
}

/// Where a pushed artifact ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRef {
    pub external_id: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub remote_version: Option<String>,
}

impl ExternalRef {
    pub fn new(external_id: impl Into<String>) -> Self {
        Self {
            external_id: external_id.into(),
            url: None,
            title: None,
            remote_version: None,
        }
    }
}

/// One artifact read from a remote system.
#[derive(Debug, Clone, PartialEq)]
pub struct Inbound {
    pub external_id: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub remote_version: Option<String>,
    pub occurred_at: Option<DateTime<Utc>>,
    pub payload: serde_json::Value,
}

/// An opaque per-connector position in a remote change feed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cursor(pub Option<String>);

impl Cursor {
    pub fn start() -> Self {
        Cursor(None)
    }

    pub fn as_deref(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

/// One page of inbound items plus the position to resume from.
#[derive(Debug, Clone, PartialEq)]
pub struct PullBatch {
    pub items: Vec<Inbound>,
    pub next_cursor: Cursor,
}

/// Whether a connector can be used right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Ok,
    /// Configured but the credential was rejected.
    NeedsAuth,
    /// Reachable check failed for a reason the user may be able to fix.
    Unavailable(String),
}
```

- [ ] **Step 6: Write lib.rs**

Create `core/crates/connectors/src/lib.rs`:

```rust
//! The connector seam.
//!
//! Every external service — a markdown vault, a webhook receiver, a calendar, a ticket
//! tracker — reaches Notewise through the traits in this crate. Nothing above it imports a
//! vendor SDK, for the same reason nothing above `notewise-ai-router` imports a model SDK.
//!
//! # Direction is a type, not a flag
//!
//! A calendar only pulls; a webhook only pushes. Rather than one trait whose methods are
//! half-unimplemented per connector, capability is expressed by which trait is implemented:
//! [`SourceConnector`] for inbound, [`SinkConnector`] for outbound, both over [`Connector`].
//!
//! # Delivery goes through the outbox
//!
//! Sinks are never called directly at the site that changed the data. Work is enqueued to
//! `connector_outbox` with an idempotency key and drained by [`Dispatcher`]. That is what
//! makes a retry update a ticket rather than file a second one.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod connector;
mod credentials;
mod dispatcher;
mod error;
mod keychain;
mod registry;
mod sinks;
mod types;

pub use connector::{Connector, SinkConnector, SourceConnector};
pub use credentials::{CredentialStore, MemoryStore, Secret};
pub use dispatcher::{Dispatcher, DispatchReport, RetryPolicy};
pub use error::{ConnectorError, Result};
pub use keychain::KeychainStore;
pub use registry::ConnectorRegistry;
pub use sinks::{MockConnector, VaultSink, WebhookSink};
pub use types::{Cursor, ExternalRef, Health, Inbound, Operation, Outbound, PullBatch};
```

The modules referenced here are created in Tasks 8–13. Until then the crate will not compile — that is expected; Step 7 runs only the error tests once `connector.rs`, `credentials.rs`, `keychain.rs`, `registry.rs`, `dispatcher.rs`, and `sinks/` exist. To keep this task independently verifiable, temporarily comment out every `mod` and `pub use` line except `error`, `types`, and their re-exports, then uncomment each as its task lands.

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p notewise-connectors error`
Expected: PASS — 4 tests.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml core/crates/connectors/
git commit -m "feat(connectors): add the crate, error taxonomy, and boundary types"
```

---

## Task 8: The connector traits and MockConnector

**Files:**
- Create: `core/crates/connectors/src/connector.rs`
- Create: `core/crates/connectors/src/sinks/mod.rs`
- Create: `core/crates/connectors/src/sinks/mock.rs`

- [ ] **Step 1: Write the failing test**

Create `core/crates/connectors/src/sinks/mock.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Operation;
    use notewise_storage::Id;

    fn outbound() -> Outbound {
        Outbound {
            node_kind: "meeting".into(),
            node_id: Id::new(),
            operation: Operation::Create,
            payload: serde_json::json!({"title": "Standup"}),
            existing: None,
        }
    }

    #[tokio::test]
    async fn push_records_what_it_was_given() {
        let mock = MockConnector::new("mock");
        let sent = outbound();

        let reference = mock.push(&sent).await.unwrap();

        assert!(!reference.external_id.is_empty());
        assert_eq!(mock.pushed(), vec![sent]);
    }

    #[tokio::test]
    async fn a_failing_mock_returns_the_configured_error() {
        let mock = MockConnector::new("mock")
            .failing_with(|| ConnectorError::Transient("simulated".into()));

        let err = mock.push(&outbound()).await.unwrap_err();
        assert!(err.is_retryable());
        assert!(mock.pushed().is_empty(), "a failed push must not record a delivery");
    }

    #[tokio::test]
    async fn mock_reports_itself_as_local() {
        let mock = MockConnector::new("mock");
        assert!(mock.is_local());
        assert_eq!(mock.id(), "mock");
        assert_eq!(mock.health().await.unwrap(), Health::Ok);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-connectors mock`
Expected: FAIL to compile — `cannot find type MockConnector`

- [ ] **Step 3: Write the traits**

Create `core/crates/connectors/src/connector.rs`:

```rust
//! The traits every external service hides behind.

use async_trait::async_trait;

use crate::error::Result;
use crate::types::{Cursor, ExternalRef, Health, Outbound, PullBatch};

/// What every connector has, regardless of direction.
///
/// `Send + Sync` so one instance can be shared across the async runtime — the dispatcher
/// holds a registry and drains several connectors concurrently.
#[async_trait]
pub trait Connector: Send + Sync + std::fmt::Debug {
    /// Stable identifier, e.g. `"vault"`, `"webhook"`, `"google_calendar"`.
    ///
    /// Persisted in `connector_outbox` and `external_items`, so changing one is a breaking
    /// change to on-disk data.
    fn id(&self) -> &str;

    fn display_name(&self) -> &str;

    /// Whether this connector keeps data on the user's machine.
    ///
    /// Surfaced in the UI so "local only" is something a user can verify rather than trust.
    fn is_local(&self) -> bool;

    async fn health(&self) -> Result<Health>;
}

/// A connector that reads from a remote system.
#[async_trait]
pub trait SourceConnector: Connector {
    async fn pull(&self, since: Cursor) -> Result<PullBatch>;
}

/// A connector that writes to a remote system.
#[async_trait]
pub trait SinkConnector: Connector {
    /// Deliver one unit of work. Implementations must treat `outbound.existing`
    /// as "this was already pushed — update it" rather than creating a second artifact.
    async fn push(&self, outbound: &Outbound) -> Result<ExternalRef>;
}
```

- [ ] **Step 4: Write MockConnector**

Prepend to `core/crates/connectors/src/sinks/mock.rs`:

```rust
//! A connector that talks to nothing.
//!
//! Public on purpose, for the reason `notewise-ai-router` keeps `MockBackend` public: a
//! boundary is only protected if it is testable. Without this, every test touching delivery
//! would need a live vendor account, those tests would get skipped, and the seam would
//! quietly erode.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::connector::{Connector, SinkConnector};
use crate::error::{ConnectorError, Result};
use crate::types::{ExternalRef, Health, Outbound};

type FailureFn = Box<dyn Fn() -> ConnectorError + Send + Sync>;

pub struct MockConnector {
    id: String,
    pushed: Mutex<Vec<Outbound>>,
    failure: Option<FailureFn>,
}

impl std::fmt::Debug for MockConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockConnector")
            .field("id", &self.id)
            .field("pushed", &self.pushed.lock().map(|p| p.len()).unwrap_or(0))
            .field("failing", &self.failure.is_some())
            .finish()
    }
}

impl MockConnector {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            pushed: Mutex::new(Vec::new()),
            failure: None,
        }
    }

    /// Make every push fail, so retry and dead-letter paths can be exercised.
    pub fn failing_with(
        mut self,
        failure: impl Fn() -> ConnectorError + Send + Sync + 'static,
    ) -> Self {
        self.failure = Some(Box::new(failure));
        self
    }

    /// Everything successfully pushed, in order.
    pub fn pushed(&self) -> Vec<Outbound> {
        self.pushed.lock().expect("mock mutex poisoned").clone()
    }
}

#[async_trait]
impl Connector for MockConnector {
    fn id(&self) -> &str {
        &self.id
    }

    fn display_name(&self) -> &str {
        "Mock"
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn health(&self) -> Result<Health> {
        Ok(Health::Ok)
    }
}

#[async_trait]
impl SinkConnector for MockConnector {
    async fn push(&self, outbound: &Outbound) -> Result<ExternalRef> {
        if let Some(failure) = &self.failure {
            return Err(failure());
        }

        let mut pushed = self.pushed.lock().expect("mock mutex poisoned");
        pushed.push(outbound.clone());

        Ok(ExternalRef::new(format!("mock-{}", pushed.len())))
    }
}
```

Create `core/crates/connectors/src/sinks/mod.rs`:

```rust
//! Sink implementations.

mod mock;

pub use mock::MockConnector;
```

Uncomment `mod connector;`, `mod sinks;`, and their `pub use` lines in `lib.rs` — but leave `VaultSink` and `WebhookSink` out of the `sinks` re-export until Tasks 12 and 13.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p notewise-connectors mock`
Expected: PASS — 3 tests.

- [ ] **Step 6: Commit**

```bash
git add core/crates/connectors/src/
git commit -m "feat(connectors): add the Connector traits and a mock sink"
```

---

## Task 9: Credentials — Secret, CredentialStore, MemoryStore

**Files:**
- Create: `core/crates/connectors/src/credentials.rs`

- [ ] **Step 1: Write the failing test**

Create `core/crates/connectors/src/credentials.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_the_secret() {
        let secret = Secret::new("ya29.super-secret-refresh-token");

        let rendered = format!("{secret:?}");
        assert!(
            !rendered.contains("ya29"),
            "a token must not reach a log through {{:?}}, got {rendered}"
        );
        assert_eq!(rendered, "Secret(redacted)");
    }

    #[test]
    fn expose_returns_the_real_value() {
        let secret = Secret::new("hunter2");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn memory_store_round_trips() {
        let store = MemoryStore::new();
        store
            .set("google_calendar", "refresh_token", &Secret::new("abc"))
            .unwrap();

        let found = store.get("google_calendar", "refresh_token").unwrap();
        assert_eq!(found.map(|s| s.expose().to_string()), Some("abc".into()));
    }

    #[test]
    fn credentials_are_namespaced_by_connector() {
        let store = MemoryStore::new();
        store.set("linear", "token", &Secret::new("l")).unwrap();
        store.set("jira", "token", &Secret::new("j")).unwrap();

        assert_eq!(
            store.get("linear", "token").unwrap().map(|s| s.expose().to_string()),
            Some("l".into())
        );
    }

    #[test]
    fn delete_removes_and_absent_delete_succeeds() {
        let store = MemoryStore::new();
        store.set("vault", "hmac", &Secret::new("k")).unwrap();

        store.delete("vault", "hmac").unwrap();
        assert!(store.get("vault", "hmac").unwrap().is_none());
        assert!(store.delete("vault", "hmac").is_ok());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-connectors credentials`
Expected: FAIL to compile — `cannot find type Secret`

- [ ] **Step 3: Write the implementation**

Prepend to `core/crates/connectors/src/credentials.rs`:

```rust
//! Credential storage.
//!
//! A long-lived refresh token has a different risk profile from a meeting summary: it grants
//! standing access to someone's calendar or tracker, and it is exactly the kind of value that
//! ends up inside a support bundle. So credentials do not go in the database — they go behind
//! this trait, whose production implementation is the OS keychain.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::Result;

/// A credential value that does not print itself.
///
/// `Debug` is implemented by hand precisely so an ordinary `{:?}` on a struct holding one
/// cannot leak it into a log line.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// Read the underlying value. Named to make call sites conspicuous in review.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(redacted)")
    }
}

/// Where connector credentials live.
///
/// Keys are namespaced by `connector_id` so two connectors cannot collide on `"token"`.
pub trait CredentialStore: Send + Sync + std::fmt::Debug {
    fn get(&self, connector_id: &str, key: &str) -> Result<Option<Secret>>;
    fn set(&self, connector_id: &str, key: &str, value: &Secret) -> Result<()>;
    /// Remove a credential. Removing an absent credential succeeds.
    fn delete(&self, connector_id: &str, key: &str) -> Result<()>;
}

/// An in-process credential store for tests.
///
/// Exists so credential-handling logic is testable on a CI machine with no unlocked keychain.
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<(String, String), Secret>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemoryStore {
    fn get(&self, connector_id: &str, key: &str) -> Result<Option<Secret>> {
        let entries = self.entries.lock().expect("credential mutex poisoned");
        Ok(entries.get(&(connector_id.to_string(), key.to_string())).cloned())
    }

    fn set(&self, connector_id: &str, key: &str, value: &Secret) -> Result<()> {
        let mut entries = self.entries.lock().expect("credential mutex poisoned");
        entries.insert((connector_id.to_string(), key.to_string()), value.clone());
        Ok(())
    }

    fn delete(&self, connector_id: &str, key: &str) -> Result<()> {
        let mut entries = self.entries.lock().expect("credential mutex poisoned");
        entries.remove(&(connector_id.to_string(), key.to_string()));
        Ok(())
    }
}
```

Uncomment `mod credentials;` and its `pub use` line in `lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p notewise-connectors credentials`
Expected: PASS — 5 tests.

- [ ] **Step 5: Commit**

```bash
git add core/crates/connectors/src/credentials.rs core/crates/connectors/src/lib.rs
git commit -m "feat(connectors): add redacting Secret and the credential store trait"
```

---

## Task 10: KeychainStore

**Files:**
- Create: `core/crates/connectors/src/keychain.rs`

- [ ] **Step 1: Write the failing test**

Create `core/crates/connectors/src/keychain.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_name_is_namespaced_per_connector() {
        let store = KeychainStore::new();
        assert_eq!(store.service_name("linear"), "com.notewise.connector.linear");
        assert_ne!(store.service_name("linear"), store.service_name("jira"));
    }

    #[test]
    #[ignore = "needs an unlocked OS keychain; CI has no login session"]
    fn round_trips_through_the_real_keychain() {
        let store = KeychainStore::new();
        let secret = Secret::new("integration-test-value");

        store.set("notewise_test", "token", &secret).unwrap();
        let found = store.get("notewise_test", "token").unwrap();
        assert_eq!(found.map(|s| s.expose().to_string()), Some("integration-test-value".into()));

        store.delete("notewise_test", "token").unwrap();
        assert!(store.get("notewise_test", "token").unwrap().is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-connectors keychain`
Expected: FAIL to compile — `cannot find type KeychainStore`

- [ ] **Step 3: Write the implementation**

Prepend to `core/crates/connectors/src/keychain.rs`:

```rust
//! `CredentialStore` over the platform keychain.
//!
//! macOS Keychain, Windows Credential Manager, and Secret Service on Linux, via the
//! `keyring` crate. A missing entry is `Ok(None)` rather than an error — "not connected yet"
//! is the normal state for most connectors, not a failure.

use keyring::Entry;

use crate::credentials::{CredentialStore, Secret};
use crate::error::{ConnectorError, Result};

#[derive(Debug, Default)]
pub struct KeychainStore;

impl KeychainStore {
    pub fn new() -> Self {
        Self
    }

    /// The keychain service name for a connector.
    ///
    /// Namespaced so that two connectors storing a key called `"token"` cannot collide, and
    /// so a user auditing their keychain can see which entry belongs to what.
    pub fn service_name(&self, connector_id: &str) -> String {
        format!("com.notewise.connector.{connector_id}")
    }

    fn entry(&self, connector_id: &str, key: &str) -> Result<Entry> {
        Entry::new(&self.service_name(connector_id), key)
            .map_err(|e| ConnectorError::Credential(e.to_string()))
    }
}

impl CredentialStore for KeychainStore {
    fn get(&self, connector_id: &str, key: &str) -> Result<Option<Secret>> {
        match self.entry(connector_id, key)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(ConnectorError::Credential(e.to_string())),
        }
    }

    fn set(&self, connector_id: &str, key: &str, value: &Secret) -> Result<()> {
        self.entry(connector_id, key)?
            .set_password(value.expose())
            .map_err(|e| ConnectorError::Credential(e.to_string()))
    }

    fn delete(&self, connector_id: &str, key: &str) -> Result<()> {
        match self.entry(connector_id, key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(ConnectorError::Credential(e.to_string())),
        }
    }
}
```

Uncomment `mod keychain;` and its `pub use` line in `lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p notewise-connectors keychain`
Expected: PASS — 1 passed, 1 ignored. The ignored test must state its reason; a green CI run must not imply the real keychain was exercised.

- [ ] **Step 5: Commit**

```bash
git add core/crates/connectors/src/keychain.rs core/crates/connectors/src/lib.rs
git commit -m "feat(connectors): store credentials in the OS keychain"
```

---

## Task 11: Registry and Dispatcher

**Files:**
- Create: `core/crates/connectors/src/registry.rs`
- Create: `core/crates/connectors/src/dispatcher.rs`

- [ ] **Step 1: Write the failing registry test**

Create `core/crates/connectors/src/registry.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sinks::MockConnector;
    use std::sync::Arc;

    #[test]
    fn resolves_a_registered_sink_by_id() {
        let mut registry = ConnectorRegistry::new();
        registry.register_sink(Arc::new(MockConnector::new("mock")));

        assert!(registry.sink("mock").is_ok());
        assert_eq!(registry.sink_ids(), vec!["mock".to_string()]);
    }

    #[test]
    fn an_unknown_id_is_an_error_not_a_panic() {
        let registry = ConnectorRegistry::new();
        let err = registry.sink("nope").unwrap_err();
        assert!(matches!(err, ConnectorError::UnknownConnector(id) if id == "nope"));
    }

    #[test]
    fn registering_the_same_id_twice_replaces_it() {
        let mut registry = ConnectorRegistry::new();
        registry.register_sink(Arc::new(MockConnector::new("mock")));
        registry.register_sink(Arc::new(MockConnector::new("mock")));

        assert_eq!(registry.sink_ids().len(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-connectors registry`
Expected: FAIL to compile — `cannot find type ConnectorRegistry`

- [ ] **Step 3: Write the registry**

Prepend to `core/crates/connectors/src/registry.rs`:

```rust
//! Which connectors this build knows about.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::connector::{SinkConnector, SourceConnector};
use crate::error::{ConnectorError, Result};

/// The set of connectors available to the dispatcher and the surfaces.
///
/// `BTreeMap` rather than `HashMap` so `sink_ids()` is stable — a UI listing connectors in
/// a different order on every launch is a bug report waiting to happen.
#[derive(Debug, Default)]
pub struct ConnectorRegistry {
    sinks: BTreeMap<String, Arc<dyn SinkConnector>>,
    sources: BTreeMap<String, Arc<dyn SourceConnector>>,
}

impl ConnectorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_sink(&mut self, sink: Arc<dyn SinkConnector>) {
        self.sinks.insert(sink.id().to_string(), sink);
    }

    pub fn register_source(&mut self, source: Arc<dyn SourceConnector>) {
        self.sources.insert(source.id().to_string(), source);
    }

    pub fn sink(&self, id: &str) -> Result<Arc<dyn SinkConnector>> {
        self.sinks
            .get(id)
            .cloned()
            .ok_or_else(|| ConnectorError::UnknownConnector(id.to_string()))
    }

    pub fn source(&self, id: &str) -> Result<Arc<dyn SourceConnector>> {
        self.sources
            .get(id)
            .cloned()
            .ok_or_else(|| ConnectorError::UnknownConnector(id.to_string()))
    }

    pub fn sink_ids(&self) -> Vec<String> {
        self.sinks.keys().cloned().collect()
    }

    pub fn source_ids(&self) -> Vec<String> {
        self.sources.keys().cloned().collect()
    }
}
```

- [ ] **Step 4: Write the failing dispatcher test**

Create `core/crates/connectors/src/dispatcher.rs` with the test module:

```rust
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
        assert_eq!(related[0].node, NodeRef::new(NodeKind::ExternalItem, item.id));
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
        assert_eq!(mock.pushed().len(), 1, "a completed row must never be pushed twice");
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
        assert!(pushed[0].existing.is_none(), "the first push has nothing to update");
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

        let row = OutboxRepository::new(&db).find_by_key("k1").unwrap().unwrap();
        assert_eq!(row.status, OutboxStatus::Pending);
        assert_eq!(row.attempts, 1);
        assert!(row.next_attempt_at > Utc::now(), "a retry must be scheduled in the future");
    }

    #[tokio::test]
    async fn an_auth_failure_dead_letters_immediately() {
        let db = Database::open_in_memory().unwrap();
        queued(&db, Id::new(), "k1");

        let mut registry = ConnectorRegistry::new();
        registry.register_sink(Arc::new(MockConnector::new("mock").failing_with(|| {
            ConnectorError::Auth { connector: "mock".into() }
        })));
        let dispatcher = Dispatcher::new(registry, RetryPolicy::default());

        let report = dispatcher.drain(&db).await.unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(report.deferred, 0, "retrying a rejected credential can never succeed");

        let row = OutboxRepository::new(&db).find_by_key("k1").unwrap().unwrap();
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
        let policy = RetryPolicy { max_attempts: 2, base_delay: Duration::seconds(0), ..RetryPolicy::default() };
        let dispatcher = Dispatcher::new(registry, policy);

        dispatcher.drain(&db).await.unwrap();
        let second = dispatcher.drain(&db).await.unwrap();

        assert_eq!(second.failed, 1);
        let row = OutboxRepository::new(&db).find_by_key("k1").unwrap().unwrap();
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
```

- [ ] **Step 5: Run test to verify it fails**

Run: `cargo test -p notewise-connectors dispatcher`
Expected: FAIL to compile — `cannot find type Dispatcher`

- [ ] **Step 6: Write the dispatcher**

Prepend to `core/crates/connectors/src/dispatcher.rs`:

```rust
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
        let claimed = OutboxRepository::new(db)
            .claim_ready(self.policy.batch_size, self.policy.lease)?;

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

        let operation = Operation::parse(&row.operation)
            .ok_or_else(|| ConnectorError::Permanent(format!("unknown operation '{}'", row.operation)))?;
        let node_kind = NodeKind::parse(&row.node_kind)
            .ok_or_else(|| ConnectorError::Permanent(format!("unknown node kind '{}'", row.node_kind)))?;

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

        let delay = err.retry_after()
            .and_then(|d| Duration::from_std(d).ok())
            .unwrap_or_else(|| self.policy.delay_for(row.attempts));

        outbox.retry_later(row.id, &err.to_string(), Utc::now() + delay)?;
        Ok(true)
    }
}
```

Uncomment `mod registry;`, `mod dispatcher;`, and their `pub use` lines in `lib.rs`.

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p notewise-connectors`
Expected: PASS — registry 3 tests, dispatcher 7 tests, plus earlier tasks'.

- [ ] **Step 8: Commit**

```bash
git add core/crates/connectors/src/
git commit -m "feat(connectors): add the registry and outbox dispatcher"
```

---

## Task 12: VaultSink

**Files:**
- Create: `core/crates/connectors/src/sinks/vault.rs`
- Modify: `core/crates/connectors/src/sinks/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `core/crates/connectors/src/sinks/vault.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Operation;
    use notewise_storage::Id;

    fn outbound(title: &str, body: &str) -> Outbound {
        Outbound {
            node_kind: "meeting".into(),
            node_id: Id::new(),
            operation: Operation::Create,
            payload: serde_json::json!({ "title": title, "markdown": body }),
            existing: None,
        }
    }

    #[tokio::test]
    async fn push_writes_a_markdown_file() {
        let dir = tempfile::tempdir().unwrap();
        let sink = VaultSink::new(dir.path());

        let reference = sink.push(&outbound("Standup", "# Standup\n\nShipped.")).await.unwrap();

        let written = std::fs::read_to_string(dir.path().join(&reference.external_id)).unwrap();
        assert!(written.contains("Shipped."));
    }

    #[tokio::test]
    async fn the_same_node_overwrites_rather_than_accumulating() {
        let dir = tempfile::tempdir().unwrap();
        let sink = VaultSink::new(dir.path());
        let mut first = outbound("Standup", "v1");
        first.operation = Operation::Update;
        let second = Outbound { payload: serde_json::json!({"title": "Standup", "markdown": "v2"}), ..first.clone() };

        sink.push(&first).await.unwrap();
        let reference = sink.push(&second).await.unwrap();

        let written = std::fs::read_to_string(dir.path().join(&reference.external_id)).unwrap();
        assert_eq!(written, "v2");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn titles_with_path_separators_cannot_escape_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        let sink = VaultSink::new(dir.path());

        let reference = sink
            .push(&outbound("../../etc/passwd", "nope"))
            .await
            .unwrap();

        assert!(!reference.external_id.contains(".."), "got {}", reference.external_id);
        assert!(dir.path().join(&reference.external_id).starts_with(dir.path()));
    }

    #[tokio::test]
    async fn a_missing_vault_directory_is_a_configuration_error_not_a_retry() {
        let sink = VaultSink::new("/nonexistent/notewise-vault-test");
        let err = sink.push(&outbound("Standup", "x")).await.unwrap_err();
        assert!(!err.is_retryable(), "retrying will not create the user's folder");
    }

    #[tokio::test]
    async fn health_reports_a_missing_directory() {
        let sink = VaultSink::new("/nonexistent/notewise-vault-test");
        assert!(matches!(sink.health().await.unwrap(), Health::Unavailable(_)));
    }

    #[test]
    fn the_vault_is_local() {
        let dir = tempfile::tempdir().unwrap();
        assert!(VaultSink::new(dir.path()).is_local());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-connectors vault`
Expected: FAIL to compile — `cannot find type VaultSink`

- [ ] **Step 3: Write the implementation**

Prepend to `core/crates/connectors/src/sinks/vault.rs`:

```rust
//! Mirror meetings into a folder of markdown files.
//!
//! The connector that needs no account, no OAuth app, and no network. Markdown because it is
//! the format that survives: a user who stops using Notewise can still read their meetings,
//! and it drops straight into Obsidian or any editor.
//!
//! Content comes from `notewise_storage::meeting_to_markdown` via the enqueued payload, so
//! this file is a destination, not a second renderer.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::connector::{Connector, SinkConnector};
use crate::error::{ConnectorError, Result};
use crate::types::{ExternalRef, Health, Outbound};

#[derive(Debug)]
pub struct VaultSink {
    root: PathBuf,
}

impl VaultSink {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Reduce a title to something safe to use as a file name.
///
/// Path separators and `..` are stripped rather than escaped: a meeting titled
/// `../../etc/passwd` must land inside the vault, not outside it. The node id is appended so
/// two meetings with the same title do not overwrite each other.
fn file_name(title: &str, node_id: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();

    let trimmed = cleaned.trim_matches(['-', ' ']).trim();
    let stem = if trimmed.is_empty() { "untitled" } else { trimmed };
    let short_id: String = node_id.chars().take(8).collect();

    format!("{stem}-{short_id}.md")
}

#[async_trait]
impl Connector for VaultSink {
    fn id(&self) -> &str {
        "vault"
    }

    fn display_name(&self) -> &str {
        "Markdown vault"
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn health(&self) -> Result<Health> {
        if self.root.is_dir() {
            Ok(Health::Ok)
        } else {
            Ok(Health::Unavailable(format!(
                "{} is not a directory",
                self.root.display()
            )))
        }
    }
}

#[async_trait]
impl SinkConnector for VaultSink {
    async fn push(&self, outbound: &Outbound) -> Result<ExternalRef> {
        let title = outbound
            .payload
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("untitled");

        let markdown = outbound
            .payload
            .get("markdown")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ConnectorError::Permanent("payload has no 'markdown' field".into()))?;

        let name = file_name(title, &outbound.node_id.to_string());
        let path = self.root.join(&name);

        // A missing vault folder is a configuration mistake, not a blip: the user moved or
        // never chose the directory. Retrying cannot fix it, so it must not be Transient.
        std::fs::write(&path, markdown).map_err(|e| {
            ConnectorError::Permanent(format!("cannot write {}: {e}", path.display()))
        })?;

        Ok(ExternalRef {
            external_id: name,
            url: Some(format!("file://{}", path.display())),
            title: Some(title.to_string()),
            remote_version: None,
        })
    }
}
```

In `core/crates/connectors/src/sinks/mod.rs`, add `mod vault;` and `pub use vault::VaultSink;`. Add `VaultSink` to the `sinks` re-export in `lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p notewise-connectors vault`
Expected: PASS — 6 tests.

- [ ] **Step 5: Commit**

```bash
git add core/crates/connectors/src/sinks/
git commit -m "feat(connectors): mirror meetings to a markdown vault"
```

---

## Task 13: WebhookSink

**Files:**
- Create: `core/crates/connectors/src/sinks/webhook.rs`
- Modify: `core/crates/connectors/src/sinks/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `core/crates/connectors/src/sinks/webhook.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Operation;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::Router;
    use notewise_storage::Id;
    use std::sync::{Arc, Mutex};

    /// Start a test receiver that records what it was sent and replies with `status`.
    async fn receiver(status: u16) -> (String, Arc<Mutex<Vec<(HeaderMap, String)>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();

        let app = Router::new().route(
            "/hook",
            post(move |headers: HeaderMap, body: String| {
                let sink = sink.clone();
                async move {
                    sink.lock().unwrap().push((headers, body));
                    axum::http::StatusCode::from_u16(status).unwrap()
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        (format!("http://{addr}/hook"), received)
    }

    fn outbound() -> Outbound {
        Outbound {
            node_kind: "decision".into(),
            node_id: Id::new(),
            operation: Operation::Create,
            payload: serde_json::json!({"text": "Ship Friday"}),
            existing: None,
        }
    }

    #[tokio::test]
    async fn push_posts_the_payload() {
        let (url, received) = receiver(200).await;
        let sink = WebhookSink::new(url, Secret::new("shh"));

        sink.push(&outbound()).await.unwrap();

        let calls = received.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].1.contains("Ship Friday"));
    }

    #[tokio::test]
    async fn deliveries_are_signed_over_the_raw_body() {
        let (url, received) = receiver(200).await;
        let sink = WebhookSink::new(url, Secret::new("shh"));

        sink.push(&outbound()).await.unwrap();

        let calls = received.lock().unwrap();
        let (headers, body) = &calls[0];
        let signature = headers
            .get("x-notewise-signature")
            .expect("a receiver must be able to tell a real delivery from anything else that can reach its URL")
            .to_str()
            .unwrap();

        assert_eq!(signature, sign(&Secret::new("shh"), body));
    }

    #[tokio::test]
    async fn a_500_is_retryable() {
        let (url, _) = receiver(500).await;
        let sink = WebhookSink::new(url, Secret::new("shh"));

        let err = sink.push(&outbound()).await.unwrap_err();
        assert!(err.is_retryable(), "a receiver having a bad minute deserves another try");
    }

    #[tokio::test]
    async fn a_400_is_permanent() {
        let (url, _) = receiver(400).await;
        let sink = WebhookSink::new(url, Secret::new("shh"));

        let err = sink.push(&outbound()).await.unwrap_err();
        assert!(!err.is_retryable(), "replaying a malformed request forever helps nobody");
    }

    #[tokio::test]
    async fn a_401_is_an_auth_error() {
        let (url, _) = receiver(401).await;
        let sink = WebhookSink::new(url, Secret::new("shh"));

        let err = sink.push(&outbound()).await.unwrap_err();
        assert!(matches!(err, ConnectorError::Auth { .. }));
    }

    #[tokio::test]
    async fn a_429_is_rate_limited() {
        let (url, _) = receiver(429).await;
        let sink = WebhookSink::new(url, Secret::new("shh"));

        let err = sink.push(&outbound()).await.unwrap_err();
        assert!(err.is_retryable());
        assert!(matches!(err, ConnectorError::RateLimited { .. }));
    }

    #[test]
    fn the_webhook_is_not_local() {
        let sink = WebhookSink::new("https://example.com/hook", Secret::new("k"));
        assert!(!sink.is_local(), "a webhook sends data off the machine");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-connectors webhook`
Expected: FAIL to compile — `cannot find type WebhookSink`

- [ ] **Step 3: Write the implementation**

Prepend to `core/crates/connectors/src/sinks/webhook.rs`:

```rust
//! POST Notewise events to a URL the user controls.
//!
//! One connector that covers the automation long tail — Zapier, Make, n8n, or a script —
//! without a bespoke integration per destination.
//!
//! Deliveries are signed with HMAC-SHA256 over the raw body. A receiver otherwise has no way
//! to distinguish a real delivery from anything else that can reach its URL.

use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::connector::{Connector, SinkConnector};
use crate::credentials::Secret;
use crate::error::{ConnectorError, Result};
use crate::types::{ExternalRef, Health, Outbound};

pub const SIGNATURE_HEADER: &str = "X-Notewise-Signature";

/// Hex HMAC-SHA256 of `body` under `secret`.
pub(crate) fn sign(secret: &Secret, body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.expose().as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[derive(Debug)]
pub struct WebhookSink {
    url: String,
    secret: Secret,
    client: reqwest::Client,
}

impl WebhookSink {
    pub fn new(url: impl Into<String>, secret: Secret) -> Self {
        Self {
            url: url.into(),
            secret,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("default reqwest client builds"),
        }
    }
}

#[async_trait]
impl Connector for WebhookSink {
    fn id(&self) -> &str {
        "webhook"
    }

    fn display_name(&self) -> &str {
        "Webhook"
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn health(&self) -> Result<Health> {
        Ok(Health::Ok)
    }
}

#[async_trait]
impl SinkConnector for WebhookSink {
    async fn push(&self, outbound: &Outbound) -> Result<ExternalRef> {
        let envelope = serde_json::json!({
            "node_kind": outbound.node_kind,
            "node_id": outbound.node_id.to_string(),
            "operation": outbound.operation.as_str(),
            "data": outbound.payload,
        });
        let body = serde_json::to_string(&envelope)?;
        let signature = sign(&self.secret, &body);

        let response = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header(SIGNATURE_HEADER, &signature)
            .body(body)
            .send()
            .await
            .map_err(|e| ConnectorError::Transient(format!("request failed: {e}")))?;

        let status = response.status();
        if status.is_success() {
            return Ok(ExternalRef {
                external_id: signature,
                url: Some(self.url.clone()),
                title: None,
                remote_version: None,
            });
        }

        // Classification is what the dispatcher branches on, so each case is decided here
        // rather than left as a generic failure.
        Err(match status.as_u16() {
            401 | 403 => ConnectorError::Auth {
                connector: "webhook".into(),
            },
            429 => {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(60);
                ConnectorError::RateLimited {
                    retry_after: Duration::from_secs(retry_after),
                }
            }
            code if (500..600).contains(&code) => {
                ConnectorError::Transient(format!("receiver returned {code}"))
            }
            code => ConnectorError::Permanent(format!("receiver returned {code}")),
        })
    }
}
```

In `core/crates/connectors/src/sinks/mod.rs`, add `mod webhook;` and `pub use webhook::{WebhookSink, SIGNATURE_HEADER};`. Add `WebhookSink` to the `sinks` re-export in `lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p notewise-connectors webhook`
Expected: PASS — 7 tests.

- [ ] **Step 5: Run the full workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add core/crates/connectors/src/sinks/
git commit -m "feat(connectors): add a signed outbound webhook sink"
```

---

## Task 14: Wire connectors into the API server

**Files:**
- Create: `core/crates/api-server/src/connectors.rs`
- Modify: `core/crates/api-server/src/state.rs`
- Modify: `core/crates/api-server/src/routes.rs`
- Modify: `core/crates/api-server/Cargo.toml`

- [ ] **Step 1: Write the failing test**

Add to `core/crates/api-server/src/connectors.rs` (create the file with this test module):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use notewise_connectors::MockConnector;
    use notewise_storage::{Database, NewOutboxEntry, OutboxRepository};

    #[test]
    fn listing_reports_registered_connectors_and_locality() {
        let mut registry = ConnectorRegistry::new();
        registry.register_sink(std::sync::Arc::new(MockConnector::new("mock")));

        let listed = describe_connectors(&registry);

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "mock");
        assert!(listed[0].is_local, "the UI shows this, so it must be accurate");
    }

    #[test]
    fn failed_deliveries_are_listable() {
        let db = Database::open_in_memory().unwrap();
        let repo = OutboxRepository::new(&db);
        let row = repo
            .enqueue(NewOutboxEntry {
                connector_id: "webhook".into(),
                node_kind: "decision".into(),
                node_id: notewise_storage::Id::new(),
                operation: "create".into(),
                payload: "{}".into(),
                idempotency_key: "k1".into(),
            })
            .unwrap();
        repo.dead_letter(row.id, "401 unauthorized").unwrap();

        let failures = describe_failures(&db, 10).unwrap();

        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].connector_id, "webhook");
        assert_eq!(failures[0].last_error.as_deref(), Some("401 unauthorized"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-api-server connectors`
Expected: FAIL to compile — `cannot find function describe_connectors`

- [ ] **Step 3: Write the handlers**

Add `notewise-connectors.workspace = true` to `[dependencies]` in `core/crates/api-server/Cargo.toml`.

Prepend to `core/crates/api-server/src/connectors.rs`:

```rust
//! Connector status and outbox inspection.
//!
//! Read-only. Connecting an account is a separate, deliberate flow — this surface exists so a
//! user can see what is configured and what failed to deliver, because a queue whose failures
//! are invisible is worse than no queue.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use notewise_connectors::ConnectorRegistry;
use notewise_storage::{Database, OutboxRepository};
use serde::Serialize;

use crate::error::ApiResult;
use crate::state::AppState;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ConnectorSummary {
    pub id: String,
    pub display_name: String,
    /// Whether this connector keeps data on the user's machine.
    pub is_local: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct FailedDelivery {
    pub id: String,
    pub connector_id: String,
    pub node_kind: String,
    pub node_id: String,
    pub attempts: u32,
    pub last_error: Option<String>,
}

pub(crate) fn describe_connectors(registry: &ConnectorRegistry) -> Vec<ConnectorSummary> {
    registry
        .sink_ids()
        .into_iter()
        .filter_map(|id| registry.sink(&id).ok())
        .map(|sink| ConnectorSummary {
            id: sink.id().to_string(),
            display_name: sink.display_name().to_string(),
            is_local: sink.is_local(),
        })
        .collect()
}

pub(crate) fn describe_failures(
    db: &Database,
    limit: u32,
) -> notewise_storage::Result<Vec<FailedDelivery>> {
    Ok(OutboxRepository::new(db)
        .list_failed(limit)?
        .into_iter()
        .map(|row| FailedDelivery {
            id: row.id.to_string(),
            connector_id: row.connector_id,
            node_kind: row.node_kind,
            node_id: row.node_id.to_string(),
            attempts: row.attempts,
            last_error: row.last_error,
        })
        .collect())
}

pub async fn list_connectors(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<ConnectorSummary>> {
    Json(describe_connectors(&state.connectors()))
}

pub async fn list_failed_deliveries(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<FailedDelivery>>> {
    // `db()` already returns the guard; there is no second `.lock()`.
    let db = state.db().await;
    Ok(Json(describe_failures(&db, 50)?))
}
```

`ApiResult` and the `StorageError` conversion already exist in
`core/crates/api-server/src/error.rs` — the `?` above needs no new `From` impl.

> **Do not hold the database guard across an `.await`.** `Database` is `Send` but not
> `Sync`, so a handler that keeps the `state.db().await` guard alive across any subsequent
> `.await` produces a non-`Send` future, and axum rejects it with an opaque
> `the trait bound `Handler<_, _>` is not satisfied` that points at the route, not the
> cause. Every handler in Tasks 14 and 15 is written so the guard is only held across
> synchronous calls — keep it that way. If you need to await something, drop the guard
> first or split the async part out from the synchronous apply.

- [ ] **Step 4: Hold the registry in AppState**

In `core/crates/api-server/src/state.rs`, add the field to `AppState`:

```rust
    /// Connectors currently registered.
    ///
    /// An `RwLock<Arc<_>>` for the same reason as `ai`: connecting a vault in one window must
    /// not block a request reading the list in another. Handlers clone the `Arc` and drop the
    /// lock immediately.
    connectors: std::sync::RwLock<Arc<notewise_connectors::ConnectorRegistry>>,
```

Initialize it in `AppState::new`:

```rust
            connectors: std::sync::RwLock::new(Arc::new(
                notewise_connectors::ConnectorRegistry::new(),
            )),
```

And add the accessors:

```rust
    pub fn connectors(&self) -> Arc<notewise_connectors::ConnectorRegistry> {
        self.connectors
            .read()
            .expect("connector registry lock poisoned")
            .clone()
    }

    /// Replace the registry — used when a connector is connected or disconnected.
    pub fn set_connectors(&self, registry: notewise_connectors::ConnectorRegistry) {
        *self
            .connectors
            .write()
            .expect("connector registry lock poisoned") = Arc::new(registry);
    }
```

- [ ] **Step 5: Register the routes**

In `core/crates/api-server/src/routes.rs`, add `mod connectors;` to the crate root (`lib.rs`) if not already present, and add these two routes alongside the existing ones:

```rust
        .route("/v1/connectors", get(crate::connectors::list_connectors))
        .route(
            "/v1/connectors/failures",
            get(crate::connectors::list_failed_deliveries),
        )
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p notewise-api-server connectors`
Expected: PASS — 2 tests.

- [ ] **Step 7: Full workspace verification**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all pass. Confirm the output before claiming the plan is done.

- [ ] **Step 8: Commit**

```bash
git add core/crates/api-server/
git commit -m "feat(api-server): expose connector status and failed deliveries"
```

---

## Task 15: Connect and disconnect

Without this the two sinks cannot be configured, so the feature is unusable.

**Files:**
- Create: `core/crates/connectors/src/config.rs`
- Modify: `core/crates/connectors/src/lib.rs`
- Modify: `core/crates/connectors/Cargo.toml`
- Modify: `core/crates/api-server/src/connectors.rs`
- Modify: `core/crates/api-server/src/routes.rs`

- [ ] **Step 1: Write the failing test**

Create `core/crates/connectors/src/config.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::MemoryStore;
    use notewise_storage::ConnectorAccountRepository;

    #[test]
    fn an_empty_database_registers_nothing() {
        let db = Database::open_in_memory().unwrap();
        let registry = build_registry(&db, &MemoryStore::new()).unwrap();
        assert!(registry.sink_ids().is_empty());
    }

    #[test]
    fn a_connected_vault_is_registered() {
        let db = Database::open_in_memory().unwrap();
        ConnectorAccountRepository::new(&db)
            .connect("vault", Some("/tmp/notes"), &[])
            .unwrap();

        let registry = build_registry(&db, &MemoryStore::new()).unwrap();
        assert_eq!(registry.sink_ids(), vec!["vault".to_string()]);
    }

    #[test]
    fn a_connected_webhook_with_a_secret_is_registered() {
        let db = Database::open_in_memory().unwrap();
        ConnectorAccountRepository::new(&db)
            .connect("webhook", Some("https://example.com/hook"), &[])
            .unwrap();
        let store = MemoryStore::new();
        store.set("webhook", SIGNING_KEY, &Secret::new("k")).unwrap();

        let registry = build_registry(&db, &store).unwrap();
        assert_eq!(registry.sink_ids(), vec!["webhook".to_string()]);
    }

    #[test]
    fn a_webhook_missing_its_secret_is_not_registered() {
        let db = Database::open_in_memory().unwrap();
        ConnectorAccountRepository::new(&db)
            .connect("webhook", Some("https://example.com/hook"), &[])
            .unwrap();

        let registry = build_registry(&db, &MemoryStore::new()).unwrap();
        assert!(
            registry.sink_ids().is_empty(),
            "signing with an empty key would produce a signature anyone could forge"
        );
    }

    #[test]
    fn a_disabled_account_is_not_registered() {
        let db = Database::open_in_memory().unwrap();
        let accounts = ConnectorAccountRepository::new(&db);
        accounts.connect("vault", Some("/tmp/notes"), &[]).unwrap();
        accounts.set_status("vault", AccountStatus::Disabled).unwrap();

        let registry = build_registry(&db, &MemoryStore::new()).unwrap();
        assert!(registry.sink_ids().is_empty());
    }

    #[test]
    fn generated_secrets_are_unique_and_long() {
        let a = generate_signing_secret();
        let b = generate_signing_secret();
        assert_ne!(a.expose(), b.expose());
        assert_eq!(a.expose().len(), 64);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p notewise-connectors config`
Expected: FAIL to compile — `cannot find function build_registry`

- [ ] **Step 3: Write the implementation**

Add `uuid.workspace = true` to `[dependencies]` in `core/crates/connectors/Cargo.toml`.

Prepend to `core/crates/connectors/src/config.rs`:

```rust
//! Building a registry from what the user has actually connected.
//!
//! The registry is derived state: `connector_accounts` plus the keychain are the source of
//! truth, so a restart rebuilds exactly what was configured, and a connector missing its
//! credential is simply absent rather than half-working.

use std::sync::Arc;

use notewise_storage::{AccountStatus, ConnectorAccountRepository, Database};
use uuid::Uuid;

use crate::credentials::{CredentialStore, Secret};
use crate::error::Result;
use crate::registry::ConnectorRegistry;
use crate::sinks::{VaultSink, WebhookSink};

/// Credential key holding a webhook's HMAC signing secret.
pub const SIGNING_KEY: &str = "signing_secret";

/// A fresh shared secret for signing webhook deliveries.
///
/// Two v4 UUIDs, hex, for 244 bits of entropy — well past what an HMAC key needs, and it
/// avoids taking a dependency on an RNG crate for one call site.
pub fn generate_signing_secret() -> Secret {
    Secret::new(format!(
        "{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    ))
}

/// Construct the registry described by the database and credential store.
///
/// A connector whose configuration is incomplete is skipped, not registered in a degraded
/// state. Half a connector is worse than none: it fails at delivery time, in the background,
/// where the user is least likely to see why.
pub fn build_registry(
    db: &Database,
    credentials: &dyn CredentialStore,
) -> Result<ConnectorRegistry> {
    let mut registry = ConnectorRegistry::new();

    for account in ConnectorAccountRepository::new(db).list()? {
        if account.status != AccountStatus::Connected {
            continue;
        }

        let Some(target) = account.account_label.as_deref() else {
            tracing::warn!(connector = %account.connector_id, "connected with no target; skipping");
            continue;
        };

        match account.connector_id.as_str() {
            "vault" => registry.register_sink(Arc::new(VaultSink::new(target))),
            "webhook" => match credentials.get("webhook", SIGNING_KEY)? {
                Some(secret) => registry.register_sink(Arc::new(WebhookSink::new(target, secret))),
                None => tracing::warn!("webhook has no signing secret; skipping"),
            },
            other => tracing::warn!(connector = %other, "no such connector in this build"),
        }
    }

    Ok(registry)
}
```

Add to `core/crates/connectors/src/lib.rs`:

```rust
mod config;
pub use config::{build_registry, generate_signing_secret, SIGNING_KEY};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p notewise-connectors config`
Expected: PASS — 6 tests.

- [ ] **Step 5: Write the failing handler test**

Add to the `tests` module in `core/crates/api-server/src/connectors.rs`:

```rust
    #[test]
    fn connecting_a_webhook_stores_a_generated_secret() {
        let db = Database::open_in_memory().unwrap();
        let store = notewise_connectors::MemoryStore::new();

        let response = apply_connect(
            &db,
            &store,
            "webhook",
            &ConnectRequest { target: "https://example.com/hook".into() },
        )
        .unwrap();

        assert!(response.signing_secret.is_some(), "the user must be shown it once");
        assert!(
            store.get("webhook", notewise_connectors::SIGNING_KEY).unwrap().is_some(),
            "the secret must be persisted or deliveries cannot be signed"
        );
    }

    #[test]
    fn connecting_a_vault_stores_no_secret() {
        let db = Database::open_in_memory().unwrap();
        let store = notewise_connectors::MemoryStore::new();

        let response = apply_connect(
            &db,
            &store,
            "vault",
            &ConnectRequest { target: "/tmp/notes".into() },
        )
        .unwrap();

        assert!(response.signing_secret.is_none());
    }

    #[test]
    fn disconnecting_removes_the_account_and_its_credential() {
        let db = Database::open_in_memory().unwrap();
        let store = notewise_connectors::MemoryStore::new();
        apply_connect(&db, &store, "webhook", &ConnectRequest { target: "https://x/y".into() })
            .unwrap();

        apply_disconnect(&db, &store, "webhook").unwrap();

        assert!(
            store.get("webhook", notewise_connectors::SIGNING_KEY).unwrap().is_none(),
            "a disconnected connector must not leave a live credential behind"
        );
    }
```

- [ ] **Step 6: Run test to verify it fails**

Run: `cargo test -p notewise-api-server connectors`
Expected: FAIL to compile — `cannot find function apply_connect`

- [ ] **Step 7: Write the handlers**

Add to `core/crates/api-server/src/connectors.rs`:

```rust
use axum::extract::Path;
use notewise_connectors::{
    build_registry, generate_signing_secret, CredentialStore, KeychainStore, SIGNING_KEY,
};
use notewise_storage::ConnectorAccountRepository;
use serde::Deserialize;

/// Where this connector should send things: a folder for the vault, a URL for the webhook.
#[derive(Debug, Deserialize)]
pub struct ConnectRequest {
    pub target: String,
}

#[derive(Debug, Serialize)]
pub struct ConnectResponse {
    pub id: String,
    /// Returned exactly once, at connect time. Notewise cannot show it again — it lives in
    /// the OS keychain, and a receiver that loses it must reconnect to get a new one.
    pub signing_secret: Option<String>,
}

pub(crate) fn apply_connect(
    db: &Database,
    credentials: &dyn CredentialStore,
    id: &str,
    request: &ConnectRequest,
) -> ApiResult<ConnectResponse> {
    if request.target.trim().is_empty() {
        return Err(ApiError::BadRequest("target must not be empty".into()));
    }

    let signing_secret = if id == "webhook" {
        let secret = generate_signing_secret();
        credentials
            .set("webhook", SIGNING_KEY, &secret)
            .map_err(|e| ApiError::Internal(format!("cannot store the signing secret: {e}")))?;
        Some(secret.expose().to_string())
    } else {
        None
    };

    ConnectorAccountRepository::new(db).connect(id, Some(&request.target), &[])?;

    Ok(ConnectResponse {
        id: id.to_string(),
        signing_secret,
    })
}

pub(crate) fn apply_disconnect(
    db: &Database,
    credentials: &dyn CredentialStore,
    id: &str,
) -> ApiResult<()> {
    // Credential first: an account row without a credential is inert, but a credential
    // without an account row is an orphan nothing will ever clean up.
    credentials
        .delete(id, SIGNING_KEY)
        .map_err(|e| ApiError::Internal(format!("cannot remove the credential: {e}")))?;
    ConnectorAccountRepository::new(db).disconnect(id)?;
    Ok(())
}

pub async fn connect_connector(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<ConnectRequest>,
) -> ApiResult<Json<ConnectResponse>> {
    let credentials = KeychainStore::new();
    let db = state.db().await;

    let response = apply_connect(&db, &credentials, &id, &request)?;
    state.set_connectors(
        build_registry(&db, &credentials)
            .map_err(|e| ApiError::Internal(format!("cannot rebuild connectors: {e}")))?,
    );

    Ok(Json(response))
}

pub async fn disconnect_connector(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    let credentials = KeychainStore::new();
    let db = state.db().await;

    apply_disconnect(&db, &credentials, &id)?;
    state.set_connectors(
        build_registry(&db, &credentials)
            .map_err(|e| ApiError::Internal(format!("cannot rebuild connectors: {e}")))?,
    );

    Ok(axum::http::StatusCode::NO_CONTENT)
}
```

Import `ApiError` alongside `ApiResult` at the top of the file.

- [ ] **Step 8: Register the routes**

In `core/crates/api-server/src/routes.rs`, add:

```rust
        .route(
            "/v1/connectors/:id",
            post(crate::connectors::connect_connector)
                .delete(crate::connectors::disconnect_connector),
        )
```

Register it **after** `/v1/connectors/failures`, so the literal path is matched before the `:id` parameter captures it.

- [ ] **Step 9: Run tests to verify they pass**

Run: `cargo test -p notewise-api-server connectors`
Expected: PASS — 5 tests.

- [ ] **Step 10: Full workspace verification**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all pass. Confirm the output before claiming the plan is done.

- [ ] **Step 11: Commit**

```bash
git add core/crates/connectors/ core/crates/api-server/
git commit -m "feat(connectors): connect and disconnect the local sinks"
```

---

## Deferred to later cycles

Stated so nobody looks for them here:

- **Enqueue call sites.** Wiring `Outbox::enqueue` into `end_meeting` and `summarize_meeting` needs the product decision of which connectors are on by default. Landing the seam first keeps that decision separate from the mechanism.
- **The dispatcher's background loop.** `Dispatcher::drain` is called explicitly in tests here. Running it on a timer belongs with the first connector a user actually enables.
- **Inbound scheduling.** `SourceConnector` is defined but has no implementations; the polling scheduler lands with the calendar connector (Spec 2).
