# Calendar and mail source connectors — design

**Date:** 2026-08-18
**Status:** draft, awaiting review
**Scope of this spec:** Spec 2 of the connector series begun in
[2026-08-14-connector-architecture-design.md](2026-08-14-connector-architecture-design.md).
Google and Microsoft calendar import, follow-up mail drafts, and the inbound engine the
seam has been missing.

---

## Why this exists

Three things in the repo are waiting on this.

`SourceConnector` has existed since Spec 0 and has no engine behind it. `Dispatcher` drains
the outbox; nothing calls `pull()`. The inbound half of the connector seam is a trait, a
cursor type, and a `PullBatch` that nothing constructs.

`series.rs` matches recurring meetings by title, and says so at
[series.rs:81-83](../../../core/crates/storage/src/repositories/series.rs#L81-L83): "Title is
the only signal available without a calendar connected." Two standups named "Standup" three
months apart are currently the same series. They are not.

`person.rs` lists calendar import as one of the discovery paths that will legitimately find
participants, and it is the only one not implemented. Speaker naming and merging landed
recently; attendee lists are the highest-quality name source available and would feed it
directly.

Underneath all three: the product promise is that a meeting is understood in context, and
right now Notewise learns about a meeting only when someone remembers to hit record.

### Why it was blocked, and why it no longer is

Spec 0's **D6** claims Google and Microsoft can both run on-device because both support PKCE
for installed applications with no client secret in the binary. The PKCE part is true and
irrelevant. The blocker is Google's *scope classification*:

- `calendar.readonly` is a **sensitive** scope — OAuth verification required. Free, but a
  real review: verified homepage, privacy policy, demo video, weeks of latency.
- Every Gmail write scope is **restricted** — verification *plus* a third-party CASA security
  assessment, billed annually.
- An unverified app in Testing status issues refresh tokens that expire every 7 days, which
  makes background calendar sync fail weekly.

So D6's conclusion was right for Microsoft and wrong for Google, for a reason it did not
consider. This spec revises it.

## Goals

- Calendar events from Google and Microsoft accounts land in the graph, on macOS, Windows,
  and Linux, at no monetary cost and behind no vendor review.
- Recurring-meeting identity comes from the provider's own recurrence id instead of title
  guessing.
- Meeting attendees become `person` rows, available to speaker naming.
- An already-generated follow-up draft reaches the user's own mailbox as a provider-side
  draft. Never sent by Notewise, and `mark_sent` is never called by this path.
- The inbound half of the connector seam gets the engine Spec 0 deliberately deferred.

## Non-goals

- **Sending mail.** Not behind a flag, not for power users. Drafts only.
- **Writing to calendars** — creating, updating, or deleting events.
- **Bot-join recording.** Phase 4, `cloud/bot-service`.
- **Contacts sync.** Attendees come from events; a contacts source is separate.
- **Multiple accounts per vendor.** `connector_accounts` is keyed by `connector_id` alone;
  multi-account is a schema change and its own spec.
- **iCloud, CalDAV, and ICS feeds.** Deferred with reasons in D7.
- **Auto-join detection.** Wave 2 of the program map; it needs native window inspection and
  carries an unrelated verification risk.

---

## Decisions

Numbering continues from Spec 0, whose decisions D1–D6 still hold except where stated.

### D7 — Google goes through a user-deployed Apps Script, not OAuth

**Revises D6.** The user pastes an open-source script we ship into their own Google account,
deploys it as a web app, and gives Notewise the deployment URL and an API key they choose.

Apps Script executes *as the deploying user*. Authorization is granted once at deploy time
through Google's own consent UI, to a script the user owns. There is no Cloud project, no
OAuth client, no verification review, no CASA assessment, and no 7-day refresh expiry. Quotas
are per-account and generous relative to polling one calendar.

One deployment serves both calendar reads and Gmail draft creation, so the setup ritual is
paid once for both capabilities.

The cost is honest and worth stating plainly: Google users perform a five-step one-time setup
that Microsoft users do not. That asymmetry is the price of not paying Google, and it is
recoverable later — if Notewise ever completes verification, a `google_oauth` connector can
be added alongside without disturbing anything, because the seam is the trait.

**Rejected — pay for CASA verification.** An annual per-year bill and a multi-week review
before the first line of calendar code ships value. Wrong order.

**Rejected — bring-your-own Cloud project.** Every user creates a GCP project, configures an
OAuth consent screen, and pastes a client id. Strictly more steps than the script, and
personal `@gmail.com` users still land in Testing status with weekly token expiry.

**Rejected — OS-native calendar (EventKit / WinRT `AppointmentStore`).** This was the leading
candidate before the Apps Script route was found. It reads whatever accounts the OS already
syncs, covering both vendors plus iCloud with no auth at all. It was rejected on total cost:
two native backends behind `unsafe` FFI, a calendar TCC grant that cannot be verified
headlessly, no Linux path, and a Windows story that depends on which Outlook the user runs —
classic Win32 Outlook may not surface in `AppointmentStore` at all. It remains the right
answer for iCloud and for zero-setup, and can be added later as a third source connector.

**Rejected — secret ICS feed URLs.** Trivially cheap and covers every provider. Google caches
published ICS for hours, which defeats pre-meeting awareness — the primary use case.
Published feeds also commonly strip attendees, which defeats the `person` enrichment. Worth
revisiting as a fallback for providers we otherwise cannot reach.

### D8 — Microsoft uses a Notewise multi-tenant PKCE app; BYO is optional

Graph is free, `Calendars.Read` and `Mail.ReadWrite` are delegated scopes a user consents to
themselves without tenant-admin approval, and publisher verification is free and optional. So
Microsoft gets what Google cannot: one click.

Notewise ships its own multi-tenant app registration. The client id is embedded in the binary,
which is correct for a public PKCE client — there is no secret to leak. Redirect is loopback.

A bring-your-own client id remains configurable for tenants whose policy requires their own
app registration. That is a settings field, not a separate connector.

**Rejected — BYO only.** AnythingLLM requires it, but AnythingLLM is a generic tool without a
first-party identity to register. Making every user do Azure paperwork for a free capability
is a worse product for no gain.

### D9 — One connector id per vendor; capabilities live in `scopes`

Ids are `"google"` and `"microsoft"`. Each is a single type implementing **both**
`SourceConnector` (calendar pull) and `SinkConnector` (draft creation). The direction-split
traits from D1 support this directly, and `ConnectorRegistry` keys separate sink and source
maps, so one `Arc` registers in both.

Per-capability opt-in rides on `ConnectorAccount.scopes`, already a `Vec<String>`:
`["calendar"]` or `["calendar", "mail"]`. A user who wants calendar without mailbox access
gets it without a second account row and without a second credential.

Ids are deliberately vendor-shaped rather than transport-shaped. `"google"` describes what the
user connected; the Apps Script bridge is an implementation detail that must be replaceable
without a data migration, because connector ids are persisted in `external_items` and
`connector_outbox`.

### D10 — Mail drafts go through the outbox; generation and state already exist

Most of this capability is already built, and this spec adds only the last hop.

`notewise_ai_router::email` already generates follow-up drafts with tone variants
(`generate_email_draft`, `generate_email_variants`, `EmailTone`), and states in its own module
docs that it drafts and never sends. `notewise_storage::EmailDraftRepository` already enforces
`Draft → Approved → Sent` with `create`, `approve`, `mark_sent`, `discard` and no method that
skips a step.

What does not exist is putting an approved draft into the user's actual mailbox. That is the
only new work here:

- Enqueue to `connector_outbox` after `approve`, idempotency key `(email_draft_id)`.
- `SinkConnector::push` creates a **provider-side draft** — Apps Script `createDraft` on Google,
  `POST /me/messages` on Graph — and returns an `ExternalRef` carrying its URL.
- The UI turns that URL into "Open draft in Gmail / Outlook".

**`mark_sent` is not called on this path.** Creating a Gmail draft is not sending, and recording
it as sent would corrupt the one state machine whose integrity the email module was built
around. The provider draft is an external artifact, so it is recorded the same way every other
external artifact is: an `external_items` row plus a `SyncedTo` edge from the `EmailDraft`, per
D11. No new state, no new column.

`mark_sent` remains reachable only if a user tells Notewise they sent it, and nothing in this
spec calls it.

For users with neither vendor connected, a new `meeting_to_eml` in `storage::export` renders the
already-generated body as RFC 5322 for the desktop shell to open in the default client. The
body comes from the existing generator; only the envelope is new.

### D11 — Event detail is an owned table; the meeting link is an edge

Per CLAUDE.md rule 4, and consistent with D3 and D4.

`external_items` records identity only — `(connector_id, external_id)`, url, title,
`remote_version`. It has nowhere to put a start time, and should not: it is the shared record
of "a thing exists elsewhere" for every connector.

Event detail therefore lives in `calendar_events`, foreign-keyed to `external_items.id` and
cascading from it. The event detail is *owned* by the external record — delete the record,
the detail goes. The association between a meeting and its event is a `SyncedTo` **edge**, as
Spec 0's inbound flow already specified. Getting this backwards would mean a schema migration
the first time an event links to something other than a meeting.

### D12 — Reconciliation is its own crate, not part of `connectors`

`connectors` is plumbing: transport, credentials, retry, idempotency. Deciding that a
recording overlapping a calendar event is *that* meeting, that an attendee is a known person,
and that two events share a series is domain logic with real rules and real edge cases.

It goes in `core/crates/calendar`, which does no I/O of its own — it takes repositories and
returns decisions. Every rule in it is unit-testable against hand-written rows with no network
and no vendor account, which is the property that matters most here.

**Rejected — a `CalendarStore` trait abstracting event sources.** Proposed during
brainstorming when native backends were still in scope. Without them, each vendor already has
exactly one implementation behind `SourceConnector`; a second trait over the same boundary
would be one abstraction per implementation, abstracting nothing.

### D13 — `Cursor` means different things per connector, and that is the point

Microsoft supports `/me/calendarView/delta` and gets a real delta link. Apps Script exposes no
change feed, so Google gets a rolling-window high-water mark and re-reads the window each
pull, made safe by `ExternalItemRepository::upsert` on `(connector_id, external_id)`.

`Cursor(Option<String>)` is opaque precisely so these can differ without a shared schema.
Cursor advances only after the batch is persisted, per Spec 0 — an interrupted pull re-reads
rather than skips.

Window: 30 days back, 90 days forward, both configurable. Backward because meetings get
recorded and reconciled after the fact; forward for pre-meeting awareness.

---

## Architecture

```
                    ┌─────────────────────────┐
   api-server ─────>│ connectors::Importer    │──> storage repositories
        │           │  (inbound engine)       │
        │           └───────────┬─────────────┘
        │                       │ SourceConnector::pull
        │           ┌───────────┴─────────────┐
        │           │ sources::Google         │  HTTPS -> user's Apps Script
        │           │ sources::Microsoft      │  HTTPS -> Graph
        │           └─────────────────────────┘
        │
        └──────────>┌─────────────────────────┐
                    │ notewise-calendar       │──> storage repositories
                    │  (reconciliation)       │──> graph edges
                    └─────────────────────────┘
```

One new crate. No native code, no platform FFI, no feature flags gating a platform SDK.

### Crates and modules

| Location | Contents | New? |
|---|---|---|
| `core/crates/calendar` | Reconciliation: event↔meeting matching, attendee→person, recurrence→series | **new crate** |
| `connectors/src/sources/mod.rs` | Source module root | new |
| `connectors/src/sources/google.rs` | Apps Script bridge; `SourceConnector` + `SinkConnector` | new |
| `connectors/src/sources/microsoft.rs` | Graph; `SourceConnector` + `SinkConnector` | new |
| `connectors/src/sources/event.rs` | Normalized wire event; per-vendor decode into it | new |
| `connectors/src/importer.rs` | Inbound engine, mirroring `dispatcher.rs` | new |
| `connectors/src/oauth.rs` | PKCE loopback flow for Microsoft | new |
| `storage/src/repositories/calendar_event.rs` | `CalendarEventRepository` | new |
| `storage/src/migrations.rs` | Schema v11 | edit |
| `storage/src/export.rs` | `meeting_to_eml` beside `meeting_to_markdown` | edit |
| `connectors/src/config.rs` | Register the two new connectors | edit |
| `api-server/src/connectors.rs` | Catalogue entries, setup routes, sync trigger | edit |
| `scripts/gapps/` | The Apps Script we ship, plus deploy instructions | new |

Dependency direction: `calendar → {storage, graph}`, `connectors → storage`,
`api-server → {connectors, calendar}`. No cycles. No SQL outside `storage`.

All under `core/`, so all MIT. `cloud/integrations` is untouched, preserving the licensing
boundary D6 established.

### Public interface

```rust
// connectors::importer
pub struct ImportReport {
    pub pulled: usize,
    pub upserted: usize,
    pub failures: Vec<(String, ConnectorError)>,
}

pub struct Importer { /* registry, window config */ }

impl Importer {
    pub fn new(registry: ConnectorRegistry, window: PullWindow) -> Self;
    /// Pull every registered source once. Mirrors `Dispatcher::drain`.
    pub async fn run(&self, db: &Database) -> Result<ImportReport>;
}

// notewise-calendar
pub struct ReconcileReport {
    pub meetings_linked: usize,
    pub people_upserted: usize,
    pub series_linked: usize,
}

/// Link unlinked events to meetings, attendees to people, events to series.
/// Idempotent: running twice changes nothing the second time.
pub fn reconcile(db: &Database, opts: ReconcileOptions) -> Result<ReconcileReport>;

/// The event a recording starting now most likely belongs to, for title and
/// attendee prefill at record time.
pub fn event_at(db: &Database, at: DateTime<Utc>) -> Result<Option<CalendarEvent>>;
```

`Importer` and `reconcile` are separate calls, always run in sequence by the caller. Splitting
them means reconciliation is testable against fixture rows with no HTTP at all, which is where
the rules that can actually be wrong live.

---

## Data model

### Schema v11

```sql
CREATE TABLE calendar_events (
    id                TEXT PRIMARY KEY,
    external_item_id  TEXT NOT NULL UNIQUE
                      REFERENCES external_items(id) ON DELETE CASCADE,
    calendar_id       TEXT NOT NULL,
    provider_source   TEXT NOT NULL,   -- 'google' | 'outlook' | 'exchange' | 'other'
    title             TEXT,
    starts_at         TEXT NOT NULL,   -- RFC 3339 UTC
    ends_at           TEXT NOT NULL,
    is_all_day        INTEGER NOT NULL DEFAULT 0,
    location          TEXT,
    join_url          TEXT,
    organizer_email   TEXT,
    recurrence_key    TEXT,            -- provider recurrence id, series identity
    status            TEXT NOT NULL,   -- 'confirmed' | 'tentative' | 'cancelled'
    updated_at        TEXT NOT NULL
);

CREATE INDEX idx_calendar_events_starts_at ON calendar_events(starts_at);
CREATE INDEX idx_calendar_events_recurrence ON calendar_events(recurrence_key)
    WHERE recurrence_key IS NOT NULL;

CREATE TABLE calendar_attendees (
    id                 TEXT PRIMARY KEY,
    calendar_event_id  TEXT NOT NULL
                       REFERENCES calendar_events(id) ON DELETE CASCADE,
    email              TEXT NOT NULL,
    display_name       TEXT,
    response_status    TEXT,           -- 'accepted' | 'declined' | 'tentative' | 'none'
    is_organizer       INTEGER NOT NULL DEFAULT 0,
    person_id          TEXT REFERENCES people(id) ON DELETE SET NULL,
    UNIQUE(calendar_event_id, email)
);

CREATE INDEX idx_calendar_attendees_person ON calendar_attendees(person_id);
```

`is_all_day` exists because an all-day event has no meaningful UTC instant, and storing one
without the flag makes a timezone bug indistinguishable from a real 00:00 meeting.

`recurrence_key` is the provider's own recurrence identifier — `recurringEventId` on Google,
`seriesMasterId` on Graph — normalized to one nullable column. This is what replaces title
matching in `series.rs`.

Attendee import writes `display_name` and `email` only. The `people` table also has
`voice_print` / `voice_dims` columns which the v6 migration documents as deliberately unwritten
pending an explicit consent decision; calendar import does not change that and must not become
the path that quietly starts populating them.

`people.email` is itself nullable, so matching an attendee to an existing person is by email
only when both sides have one, and creates a new person otherwise. Merging is already a
first-class operation and is the right place to fix a wrong split.

`person_id` is nullable and `ON DELETE SET NULL`: an attendee is a fact about an event and
survives the person record being merged or removed. Speaker merging already exists and will
repoint these.

### Graph kinds

No new kinds. Reuses `NodeKind::ExternalItem` and the `SyncedTo` edge, as Spec 0's inbound
flow specified. The edge direction is meeting → external item, matching how a pushed ticket is
recorded, so traversal is uniform regardless of which side originated.

### Repository surface

```rust
impl CalendarEventRepository<'_> {
    pub fn upsert(&self, new: NewCalendarEvent) -> Result<CalendarEvent>;
    pub fn get_by_external_item(&self, id: Id) -> Result<Option<CalendarEvent>>;
    pub fn between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<CalendarEvent>>;
    pub fn overlapping(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<CalendarEvent>>;
    pub fn unlinked(&self, limit: usize) -> Result<Vec<CalendarEvent>>;
    pub fn by_recurrence_key(&self, key: &str) -> Result<Vec<CalendarEvent>>;
    pub fn replace_attendees(&self, event_id: Id, attendees: &[NewAttendee]) -> Result<()>;
    pub fn attendees(&self, event_id: Id) -> Result<Vec<Attendee>>;
}
```

`replace_attendees` rather than incremental upsert: an attendee removed from an invitation
must disappear, and a diff against the remote list is more code than a delete-and-insert
inside the transaction that writes the event.

---

## Data flow

### Import

Follows the sequence Spec 0's inbound section fixed, with the persistence step expanded:

```
Importer::run
  └─> for each registered source:
        ├─> read connector_accounts.cursor
        ├─> SourceConnector::pull(cursor)
        └─> per Inbound item, in one transaction:
              ├─> ExternalItemRepository::upsert          (identity)
              ├─> decode payload -> NewCalendarEvent
              ├─> CalendarEventRepository::upsert         (detail)
              └─> replace_attendees
        └─> after the batch commits: set_cursor(next_cursor)
```

A pull that fails partway leaves the cursor untouched and re-reads on the next run. Upsert on
`(connector_id, external_id)` makes the re-read a no-op rather than a duplicate.

A source failure is recorded against that connector and does not abort the others, mirroring
`Dispatcher::drain`.

### Reconciliation

Separate pass, no network:

```
reconcile
  ├─> for each unlinked, non-cancelled event:
  │     ├─> find meetings overlapping [starts_at, ends_at]
  │     ├─> if exactly one exceeds the overlap threshold -> graph.connect(meeting, SyncedTo, item)
  │     ├─> if several -> leave unlinked, record ambiguity
  │     └─> if none -> leave unlinked. Normal, not an error.
  ├─> for each attendee without person_id:
  │     └─> PersonRepository upsert by email -> set person_id
  └─> for each event with recurrence_key:
        └─> attach to series by recurrence_key, creating the series if absent
```

Overlap threshold: a meeting must overlap the event by the greater of 50% of the event's
duration or five minutes. Recordings routinely start late and run long, so exact bounds match
nothing; unconditional nearest-match would attach a recording to the wrong entry in a
back-to-back afternoon.

Ambiguity is left unresolved rather than guessed. A wrong link is worse than no link, because
it silently attributes decisions and action items to the wrong meeting and the user has no
reason to look.

Series migration: `series.rs` keeps title matching for meetings with no linked event, so
connecting a calendar improves matching without invalidating history.

### Mail draft

```
user requests follow-up
  └─> body from ai_router::email::generate_email_draft (already implemented)
  └─> EmailDraftRepository::create -> approve   (already implemented)
  └─> enqueue to connector_outbox, idempotency key = (meeting_id, 'email_draft')
        └─> Dispatcher -> SinkConnector::push
              ├─> google:    Apps Script createDraft   -> ExternalRef{ url }
              ├─> microsoft: POST /me/messages         -> ExternalRef{ url }
              └─> neither connected: meeting_to_email_draft -> .eml -> shell open
        └─> external_items row + SyncedTo edge from the EmailDraft
        └─> UI shows "Open draft" from ExternalRef.url
           (mark_sent is NOT called; a provider draft is not a sent message)
```

`existing` on a re-push updates the same draft rather than creating a second one, which is
D2's whole purpose.

### Setup

**Google** — six steps, once: open the script editor, create a project, paste the script from
`scripts/gapps/`, set an API key of their choosing, deploy as web app executing as themselves,
paste deployment URL and key into Notewise. Notewise then calls a `version` endpoint to
confirm the bridge answers and matches a supported contract version.

**Microsoft** — click Connect, consent in the system browser, PKCE loopback completes, refresh
token to `KeychainStore`.

Both end at `AccountStatus::Connected` with `scopes` recording chosen capabilities. Per
`build_registry`, an account missing its credential is skipped entirely rather than registered
degraded.

---

## Error handling

`ConnectorError` already carries most of what this needs — `Auth { connector }`,
`RateLimited { retry_after }`, `Transient`, `Permanent`, `NotConfigured`, and a `Serde`
conversion. Only one variant is genuinely new.

| Condition | Variant | Surface behaviour |
|---|---|---|
| Bridge URL unreachable | `Transient` (existing) | `Health::Unavailable(reason)`, retry with backoff |
| Bridge rejects API key | `Auth` (existing) | `AccountStatus::NeedsReauth`, prompt re-setup |
| Graph token refresh fails | `Auth` (existing) | `NeedsReauth` |
| Apps Script quota exhausted | `RateLimited` (existing) | Skip cycle, no cursor advance |
| Graph 429 | `RateLimited` (existing) | Honour `Retry-After` |
| Payload fails to decode | `Serde` (existing, via `#[from]`) | Skip the item, log external id, continue batch |
| Bridge contract too old | **`BridgeVersion { found, required }`** (new) | Prompt to redeploy the script, naming the version |
| Delta token expired (410) | not an error | Reset cursor, full window re-read |

Reusing `Auth` rather than adding `AuthRejected` matters beyond tidiness: `ConnectorError`
already classifies retryability in one place (`is_retryable`, `retry_after`), and a parallel
auth variant would have to be added to that logic or would silently default to the wrong
branch.

`BridgeVersion` is the one addition, and it exists because a user-deployed script is a version
we do not control. Users will still be running a year-old deployment; the failure must name the
fix rather than surface as a decode error.

Graph's 410 on an expired delta token is recovery, not failure — drop the cursor and re-read
the window, which upsert makes harmless.

No `unwrap()` in any of it; each crate keeps its `thiserror` enum, and `api-server` translates
at the boundary.

## Testing

What runs in CI, with no account and no network:

- **Both connectors against a stub HTTP server.** Recorded response fixtures for Apps Script
  and Graph, including error shapes — 401, 429, 410, malformed JSON, an all-day event, a
  cancelled event, a recurring instance.
- **Decode tests** per vendor: real-shaped payloads to `NewCalendarEvent`, including
  timezone-bearing and all-day events.
- **`Importer` against `MockConnector`** extended to implement `SourceConnector`: cursor
  advance only after commit, partial-failure isolation, re-pull idempotence.
- **Reconciliation** entirely on hand-written rows: exact overlap, partial overlap above and
  below threshold, two candidates, zero candidates, cancelled event, all-day event,
  attendee-to-person, recurrence-to-series, and idempotence on a second run.
- **Schema v11** forward migration, cascade behaviour on external item delete, and person
  delete setting `person_id` null rather than removing the attendee.
- **PKCE flow** construction and redirect handling with a local listener; no real IdP.

Marked `#[ignore]` with a reason, per CLAUDE.md rule 6:

- Anything requiring a real deployed Apps Script.
- Anything requiring a real Microsoft tenant consent.
- The end-to-end draft-creation path against either provider.

A green CI run must not imply that a real Google account was ever contacted. It implies the
decode, cursor, reconciliation, and error-translation logic is correct, which is what unit
tests can honestly establish.

## What this delivers

1. `Importer` — the inbound engine, completing the seam Spec 0 shaped.
2. `google` connector: calendar pull and Gmail draft creation via a user-deployed Apps Script.
   No Cloud project, no verification, no recurring cost.
3. `microsoft` connector: calendar pull via Graph delta and Outlook draft creation, one-click
   PKCE against a Notewise multi-tenant registration.
4. `core/crates/calendar` — reconciliation, fully tested without network or vendor accounts.
5. Schema v11 plus `CalendarEventRepository`.
6. `meeting_to_eml` in `storage::export` — the RFC 5322 fallback for unconnected users,
   wrapping the body the existing `ai-router::email` generator already produces.
7. `api-server` routes: catalogue entries, both setup flows, manual sync trigger, sync status.
8. `scripts/gapps/` — the shipped Apps Script and its deploy guide.

Works on macOS, Windows, and Linux, because none of it is platform code.

## Risks and open questions

- **Apps Script quotas under polling.** Per-account limits are generous for one calendar but
  are not documented as a contract. Mitigation: conservative default poll interval,
  `RateLimited` handling, and a manual sync that does not depend on the timer.
- **Script version drift.** The `version` endpoint and `BridgeVersion` error address
  detection; a smooth *upgrade* path for a deployed script remains an open question.
- **Google's policy may change.** The scope tiers and CASA requirements described here should
  be re-verified before implementation begins. If Google restricts Apps Script's mail access,
  the calendar half survives and the draft half falls back to `.eml`.
- **Overlap threshold is a guess.** 50%-or-five-minutes is reasoned, not measured. It should
  be revisited against real recordings once there are any, and it is a single constant.
- **Timezone handling for all-day events** is the most likely source of an off-by-one-day bug
  and deserves the most test attention.
