# Data model

## Entities

```
Workspace ──has many── Project ──has many── Meeting · Note · Ticket

Meeting ──┬── TranscriptSegment (speaker, text, start_ms, end_ms, confidence)
          └── Summary ──┬── Decision (text, reasoning, decided_at)
                        └── ActionItem (owner, due_at, status)

Note · Ticket · EmailDraft · Notification
```

Foreign keys model **ownership**; graph edges model **association**. See
[core-engine.md](core-engine.md) for why that split matters.

## Migrations are append-only

Never edit a shipped migration — add a new one. The applied version lives in SQLite's
`user_version` pragma, which avoids a bootstrap problem: no table needs to exist before the
version can be read.

Each migration runs in a transaction, so a failure part-way leaves the database at the
previous version rather than half-migrated. A database written by a **newer** build is
refused with `SchemaTooNew` rather than being opened and corrupted.

## Two state machines worth knowing

**`WorkStatus`** (`todo`, `in_progress`, `done`, `cancelled`) — `is_open()` covers the first
two. Used by both action items and tickets.

**`DraftStatus`** (`draft`, `approved`, `sent`, `discarded`) — this one is enforced, not
merely documented:

- `approve()` only moves `draft → approved`
- `mark_sent()` only moves `approved → sent`
- There is **no method** that reaches `sent` from `draft`
- A `sent` draft cannot be discarded — a sent email cannot be un-sent

A wrong auto-send is the highest-consequence failure in this product, so the constraint lives
in the type's API rather than in a comment asking callers to be careful.

## Search

FTS5, kept in sync by triggers on `notes`, `tickets`, and `transcript_segments`. A standalone
FTS table rather than external-content, because external-content FTS needs stable integer
rowids and ours are text UUIDs.

User input is treated as a **literal phrase**, not FTS5 syntax. Passing raw input through lets
stray punctuation produce a syntax error instead of zero results — a confusing failure for
something typed into a search box.

## Encryption at rest

Behind the `sqlcipher` feature, **off by default**: it swaps bundled SQLite for bundled
SQLCipher, which is substantially slower to compile.

`Database::open_encrypted` returns `EncryptionUnavailable` when the feature is absent rather
than silently opening the database unencrypted. Failing loudly is the only safe behaviour
when a caller asked for encryption and cannot have it.
