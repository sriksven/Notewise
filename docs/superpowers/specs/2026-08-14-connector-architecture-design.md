# Connector architecture — design

**Date:** 2026-08-14
**Status:** approved, ready for implementation planning
**Scope of this spec:** Spec 0 + Spec 1 below. Specs 2–4 are described here only so the
seam is designed to hold them; they are separate design cycles.

---

## Why this exists

Notewise has one rule that makes "local or cloud, your choice" a compiler-enforced property
rather than a README claim: nothing outside `core/crates/ai-router` talks to a model
provider. There is no equivalent rule for Linear, Google Calendar, Slack, or a markdown
vault. Nothing in `docs/architecture/` plans one.

Without a seam, the first four connectors become four bespoke integrations with four retry
strategies, four token stores, and four ways of remembering what was already pushed. The
fourth one is where you discover the retry path files duplicate tickets.

This spec defines that seam.

## Goals

- One interface every external service hides behind, mirroring the `ai-router` discipline.
- Outbound delivery that is idempotent and survives restarts.
- External artifacts discoverable through the existing graph, not a side channel.
- Credentials that are not sitting in the SQLite file.
- A licensing boundary between MIT `core/` and BSL `cloud/` that is a rule, not a judgment
  call per connector.

## Non-goals

- Bidirectional sync with conflict resolution. Pushes are one-way; pulls are read-only.
  Two-way sync is a much larger problem and is not needed by any Phase 2 connector.
- Any specific vendor connector beyond the two local ones. Vendors are later cycles.
- Write access for MCP agents. Related, but a separate trust decision (Spec 3).

---

## Decomposition

The five connector categories are not peers — four are backends, one is the thing they plug
into. Work splits accordingly:

| Spec | Scope | License / phase |
|---|---|---|
| **0** | Connector seam: traits, outbox, credentials, graph kinds, schema v5 | MIT `core/` — now |
| **1** | Markdown vault sink + outbound webhook sink | MIT `core/` — now |
| 2 | Calendar source (Google, Microsoft) over PKCE | MIT `core/` — Phase 2 |
| 3 | MCP write scope | MIT `core/` — independent |
| 4+ | Vendor long tail: PM, comms, CRM, capture sources, hosted STT | mixed — Phase 3 |

Specs 0 and 1 ship as one cycle. A trait with no implementations cannot be verified, and the
two local sinks exercise the full path — enqueue, deliver, record, retry — without an OAuth
app standing in the way.

Spec 3 is on this list because it was asked for alongside the others, but it does not depend
on the connector crate. It is a trust decision about the `mcp-server` surface.

---

## Decisions

Each of these was chosen against a named alternative, recorded so the reasoning survives.

### D1 — Direction-split traits, not one trait with capability flags

`AiBackend` gets away with a single trait because every model backend genuinely performs all
four operations. Connectors do not: calendar only pulls, a webhook only pushes, a vault only
mirrors. A single trait would leave every implementation stubbing half its methods, and
"does this support push" would become a runtime check.

Splitting by direction makes capability a compile-time fact — expressed by which trait you
implement.

**Rejected:** one `Connector` trait with a `capabilities()` bitmask.

### D2 — Outbound work goes through an outbox table

Pushes fail. A retry that is not idempotent files the same Linear issue twice. An outbox
gives at-least-once delivery, survives process restarts, makes pending work inspectable, and
makes the delivery path testable without network access.

**Rejected:** direct calls at the event site, with retries in memory.

### D3 — External artifacts are graph nodes

`NodeKind::ExternalItem` plus `EdgeKind::SyncedTo`. A decision that became Linear ENG-412 is
reachable from `find_related` in one traversal, so agents and the UI get external links
through the mechanism they already use.

`kinds.rs` documents that adding a variant needs no schema migration — the edge table stores
kinds as strings — so this is cheap now and expensive to retrofit later.

**Rejected:** a `sync_state` table queried separately by each surface.

### D4 — One node kind for all external artifacts

A calendar event is an `ExternalItem` whose connector is `google_calendar`. A separate
`CalendarEvent` kind would be a second mechanism for the same idea.

### D5 — Credentials live in the OS keychain behind a trait

A long-lived Google refresh token has a materially different risk profile from a meeting
summary. It is also exactly the kind of thing that ends up inside a support bundle. A
`CredentialStore` trait keeps the OS-specific code at one boundary and keeps tests honest via
an in-memory implementation.

**Rejected:** the existing `app_settings` table.

### D6 — Vendor connectors run locally where PKCE allows

Google and Microsoft both support OAuth for installed applications using PKCE, with no client
secret in the binary. Calendar, Linear, and GitHub can therefore run entirely on-device in
MIT `core/`, with no Notewise account required.

`cloud/integrations` (BSL) is reserved for vendors that genuinely require a confidential
client or a publicly reachable webhook receiver — Salesforce, Slack events. That is a rule,
which matters because `CLAUDE.md` warns that moving code between `cloud/` and `core/`
relicenses it.

**Rejected:** brokering every vendor through the cloud, which would make external connectors
require an account — including the local-first ones.

---

## Architecture

New crate `core/crates/connectors`, added to `members` in the root `Cargo.toml` and to
`[workspace.dependencies]` as `notewise-connectors`.

Dependencies point downward only: it depends on `storage` and `graph`, and on no surface.
Surfaces depend on it.

```
apps/desktop ─┬─ api-server ─┐
apps/cli ─────┤              ├─> connectors ─> graph ─> storage
              └─ mcp-server ─┘        └─> CredentialStore (OS keychain)
```

### Public interface

```rust
#[async_trait]
pub trait Connector: Send + Sync + std::fmt::Debug {
    /// Stable identifier, e.g. "vault", "webhook", "google_calendar".
    /// Persisted in the outbox and in external_items — changing one is a breaking change.
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    /// Whether this connector keeps data on the user's machine.
    fn is_local(&self) -> bool;
    async fn health(&self) -> Result<Health>;
}

#[async_trait]
pub trait SourceConnector: Connector {
    async fn pull(&self, since: Cursor) -> Result<PullBatch>;
}

#[async_trait]
pub trait SinkConnector: Connector {
    async fn push(&self, outbound: &Outbound) -> Result<ExternalRef>;
}

pub trait CredentialStore: Send + Sync + std::fmt::Debug {
    fn get(&self, connector_id: &str, key: &str) -> Result<Option<Secret>>;
    fn set(&self, connector_id: &str, key: &str, value: &Secret) -> Result<()>;
    fn delete(&self, connector_id: &str, key: &str) -> Result<()>;
}
```

`is_local()` carries over from `AiBackend` deliberately: the UI can then show which
connectors keep data on-device as something a user can verify rather than trust.

Supporting types: `Health`, `Cursor`, `PullBatch`, `Inbound`, `Outbound`, `ExternalRef`,
`Operation` (`Create` | `Update` | `Delete`), and `Secret` — a wrapper whose hand-written
`Debug` renders `Secret(redacted)`, so a token cannot reach a log through an ordinary `{:?}`.

`Secret` deliberately implements **no** `Display`. An earlier draft of this spec said `Display`
should redact too; not implementing it at all is the stronger guarantee. A redacting `Display`
would give `{}` a second, easy-looking spelling that a maintainer might reach for expecting
either behaviour — and `Display` conventionally shows the real value, so whichever it did
would surprise someone. With no impl, `{}` fails to compile and the only route to the raw
value is the conspicuously named `.expose()`, which is the friction the type exists to create.

### Components

| Component | Responsibility |
|---|---|
| `ConnectorRegistry` | Holds enabled connectors, resolves by id, reports which are configured |
| `Outbox` | Enqueue, claim-with-lease, complete, fail, dead-letter |
| `Dispatcher` | Drains the outbox, applies backoff, writes results back to the graph |
| `KeychainStore` | `CredentialStore` over macOS Keychain / Windows Credential Manager / Secret Service |
| `MemoryStore` | `CredentialStore` for tests |
| `MockConnector` | Public. Records pushes, replays canned pulls |

`MockConnector` is public for the reason `ai-router` states about `MockBackend`: a boundary is
only protected if it is testable. Without it, every test touching a connector needs a live
vendor account, those tests get skipped, and the seam quietly erodes.

---

## Data model

### Graph kinds

In `core/crates/graph/src/kinds.rs`:

- `NodeKind::ExternalItem` → `"external_item"`, appended to `NodeKind::ALL`
- `EdgeKind::SyncedTo` → `"synced_to"`, appended to `EdgeKind::ALL`
- `all_lists_are_exhaustive` counts go from 11/8 to 12/9

Doc comment for `SyncedTo`: *"This node is mirrored in an external system, e.g. action item →
Linear issue."*

### Schema v5

Appended to `MIGRATIONS` in `core/crates/storage/src/migrations.rs`. Migrations are
append-only; v1–v3 are untouched.

```sql
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
```

`connector_accounts` holds no tokens. Tokens live in the keychain, addressed by
`connector_id`.

**Ownership vs association.** An `external_items` row is owned by nothing in Notewise — it
records a thing that lives elsewhere. So it gets a table with no owning foreign key, and is
associated into the object graph by `SyncedTo` edges. This satisfies rule 4 rather than
bending it.

### Repositories

SQL stays in `storage` (rule 3). Three new repositories in
`core/crates/storage/src/repositories/`: `ExternalItemRepository`, `ConnectorAccountRepository`,
`OutboxRepository`. The `connectors` crate composes repository calls and writes no SQL.

---

## Data flow

### Outbound

```
domain event
  └─> Outbox::enqueue(connector, node, operation, payload)
        idempotency_key = hash(connector_id, node_kind, node_id, operation)
          └─> Dispatcher claims a ready row (lease guards concurrent drains)
                └─> SinkConnector::push(&Outbound) -> ExternalRef
                      ├─> upsert external_items
                      ├─> graph.connect(node, SyncedTo, external_item)
                      └─> mark outbox row complete
```

The idempotency key is the point of the whole table. Re-enqueueing an already-delivered
action item finds the existing `external_items` row and issues an update, rather than filing a
second ticket. Uniqueness is enforced by index, not by application-level checking.

**This is at-least-once delivery, not exactly-once, and the gap is not closable here.** Three
defences stack: the unique `idempotency_key` stops a *retry* from re-queueing; the atomic
claim stops two dispatchers from taking the same row; and `existing_ref` stops a genuinely
new enqueue for an already-synced node from creating a second artifact. What none of them
covers is a crash *after* the sink's remote call succeeds and *before* the local
`external_items` row and its `SyncedTo` edge are both written. On redelivery, `existing_ref`
traverses edges, finds nothing, and the sink creates a duplicate.

Wrapping the three writes in `record_success` in one transaction narrows that window to the
interval between the remote side effect and the first local write. It cannot eliminate it: no
local transaction can be atomic with a side effect on someone else's server. Closing it
completely needs the remote system to accept an idempotency key of its own — which Linear,
Stripe, and GitHub all support and a plain webhook receiver does not. Per-connector, not
architectural.

**There is no event bus.** "Domain event" above means an explicit `Outbox::enqueue` call at
the site that changed the data — `end_meeting` and `summarize_meeting` in `api-server`, and
the equivalent `cli` commands. An event bus was considered and rejected: it would be a second
dispatch mechanism alongside the outbox, and the outbox already provides the durability and
ordering a bus would be introduced to get. Enqueue is cheap and synchronous; delivery is not.

### Inbound

Spec 0 defines the inbound types and the `SourceConnector` trait so the seam is shaped to
hold them, but **builds no scheduler** — Specs 0 and 1 contain no source connectors, and a
scheduler with nothing to schedule is untestable. The polling scheduler lands with Spec 2,
the first source connector.

```
scheduler tick  (Spec 2)
  └─> SourceConnector::pull(since: cursor)
        └─> for each Inbound item:
              ├─> upsert external_items
              ├─> match to an existing node (calendar events match meetings by time overlap)
              ├─> graph.connect(node, SyncedTo, external_item) when matched
              └─> advance connector_accounts.cursor
```

Cursor advances only after the batch is persisted, so an interrupted pull re-reads rather than
skips. Unmatched inbound items are retained unlinked; a calendar event with no meeting yet is
normal, not an error.

---

## Error handling

A `thiserror` enum per rule 5, with variants that drive retry policy rather than merely
describing failure:

```rust
pub enum ConnectorError {
    Auth { connector: String },            // stop retrying; surface "reconnect" in UI
    RateLimited { retry_after: Duration }, // honor the header
    Transient(String),                     // exponential backoff, capped attempts
    Permanent(String),                     // dead-letter immediately
    Credential(#[from] CredentialError),
    Storage(#[from] StorageError),
    Graph(#[from] GraphError),
}
```

Classification is the substance here. Retrying an auth failure forever burns quota and can
never succeed, so `Auth` marks the account `needs_reauth` and stops. `RateLimited` respects
the vendor's own backoff rather than guessing.

Rows that exhaust their attempts move to `status = 'failed'` and remain listable. Silently
dropping a failed push is worse than never having attempted it.

No `unwrap()` in the crate. Surfaces translate: `api-server` to HTTP codes, `cli` to exit
codes.

---

## Testing

Fully verifiable, no `#[ignore]`:

- Outbox: same idempotency key twice yields one `external_items` row; backoff schedule;
  lease prevents double-claim; exhausted attempts dead-letter.
- Vault sink against a `tempfile::TempDir` — real files, no network.
- Webhook sink against a local `axum` test server, including a 500 path that retries and a
  429 path that honors `Retry-After`.
- Graph: `SyncedTo` edges surface through `find_related` with the right `via`.
- Kind round-trip tests extend automatically via the existing `ALL` loops.
- Token refresh logic against a mock HTTP server.

`#[ignore]` with a stated reason, per rule 6:

- The real OAuth browser redirect — needs a live vendor app and a user present.
- Real keychain reads — CI has no unlocked login keychain. `MemoryStore` covers the logic, so
  the ignored tests cover only the OS boundary itself.

A green CI run must not imply that a real Google account was ever contacted.

---

## What Spec 0 + 1 delivers

1. `core/crates/connectors` with `Connector`, `SourceConnector`, `SinkConnector`,
   `CredentialStore`, `MockConnector`, `ConnectorRegistry`, `Outbox`, `Dispatcher`.
2. Schema v5 and three repositories in `storage`.
3. `ExternalItem` and `SyncedTo` in `graph`.
4. `KeychainStore` for macOS and Windows; Secret Service for Linux.
5. **Vault sink** — mirrors meetings to a user-chosen folder as markdown, reusing
   `storage::export`, which already renders it.
6. **Webhook sink** — POSTs a JSON payload on `meeting.ended`, `decision.created`,
   `action_item.created`, signed with an HMAC-SHA256 header over the raw body. The shared
   secret is generated by Notewise when the endpoint is configured, shown to the user once,
   and stored via `CredentialStore` like any other credential. Receivers need a way to tell a
   real delivery from anything else that can reach their URL.
7. `api-server` routes to list connectors, connect/disconnect, and inspect outbox state,
   including failed rows.

Neither sink needs an OAuth app, a vendor account, or a network dependency in CI. Both are
genuinely useful on their own: the vault is the strongest local-first differentiator against
Otter and Fireflies, and the webhook covers the automation long tail through Zapier, Make, and
n8n without a bespoke integration each.
