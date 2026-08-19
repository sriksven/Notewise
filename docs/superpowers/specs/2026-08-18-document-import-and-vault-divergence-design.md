# Document import and vault divergence — design

**Date:** 2026-08-18
**Status:** draft, awaiting review
**Scope:** Spec 10 of the program map. Bringing external documents in, and resolving vault files the
user has edited.

---

## Why this exists, and why it is two things

AnythingLLM's "live document sync" watches source documents and re-embeds them when they change. The
naive translation to Notewise is "make the vault bidirectional," and that translation is wrong. Two
genuinely different needs get conflated:

**Bringing outside material in.** A user's meetings are not their only context. A design doc, a spec,
a set of notes written elsewhere — indexing those makes every grounded answer better, and Notewise has
no way to import any of it.

**Resolving a vault file the user edited.** `vault.rs` already detects this: it stores a SHA-256
fingerprint in `ExternalRef.remote_version` and, since `b6e9c3f fix(connectors): the vault overwrote
the edits it promised were yours`, refuses to overwrite a file whose content no longer matches. That
bug is fixed. What is missing is telling the user, who currently gets silence and a mirror that
quietly stopped updating.

These need different mechanisms and different decisions, so this spec keeps them separate.

## Goals

- Import documents from a watched folder as workspace material, indexed like everything else.
- Re-import them when they change, without re-embedding what did not.
- Tell the user when a vault file has diverged, and offer a resolution.
- Reuse Spec 1's `Importer` rather than building a second inbound path.

## Non-goals

- **Bidirectional sync of meeting exports.** See V1. This is the thing that sounds obvious and is not.
- **Remote document sources** — Confluence, GitHub, Google Drive. Folder-watching first; those are
  connectors and belong in the vendor long tail.
- **Editing imported documents inside Notewise.** They are read-only mirrors of files someone else
  owns.
- **Automatic conflict merging.** See V4.

---

## Decisions

### V1 — Meeting-export markdown is not imported back

The vault renders a meeting to markdown via `meeting_to_markdown`. Reversing that is rejected.

The mapping is lossy and ambiguous in both directions. A meeting is a transcript with timings and
speakers, a summary with a model attribution, decisions, and action items with owners and statuses.
The markdown is a *rendering* of that. Parsing an edited rendering back means deciding whether a
changed sentence edits the summary, corrects a transcript segment, or is a human annotation belonging
to none of them — and getting it wrong silently rewrites the record of what was actually said, which
is the one thing this product claims to be reliable about.

The vault's own module docs describe it as "a destination, not a second renderer." Making it a source
inverts that for no clear gain: a user who wants to annotate a meeting has the block note editor, which
is already linked to the meeting and already indexed.

**What happens instead:** divergence is surfaced (V3) and the user chooses. No parsing.

### V2 — Document import is a `SourceConnector`, reusing Spec 1's `Importer`

A watched folder is a connector with id `"documents"` implementing `SourceConnector`. Files become
`external_items` plus a `documents` detail table, and `Importer` drains it exactly as it drains
calendars.

The alternative — a bespoke file-watching path — would duplicate cursor handling, idempotent upsert,
and failure isolation that `Importer` already has. The connector seam is the right shape for "read
things from outside," and a folder is outside.

The cursor holds the last scan's high-water modification time, the same rolling-window shape Google's
calendar uses for the same reason: no change feed exists.

**Depends on Spec 1** for `Importer`. This spec cannot land first.

### V3 — Divergence is surfaced as a decision, not resolved automatically

When the vault sink refuses a write because the fingerprint changed, it records that and the user is
told: this file was edited outside Notewise, and the mirror has paused for it.

Three offered resolutions, all explicit: **keep the file** and stop mirroring that meeting, **overwrite
the file** with the current render, or **copy the file's content into a linked note** and resume
mirroring. The third is the one that preserves the user's writing without pretending to parse it.

Silence is the current behaviour and the worst option. A user who edited a meeting file in Obsidian and
later notices it never updates again has been failed quietly, which is exactly the complaint `b6e9c3f`
was fixing at a lower level.

### V4 — No automatic merging, ever

If a document changes in the watched folder, the new content replaces the old mirror and it is
re-indexed. If a vault file diverges, the user chooses. Nothing merges.

Three-way merge on prose without a common ancestor produces plausible text that neither side wrote.
For meeting records that is unacceptable; for imported documents it is unnecessary, because the file on
disk is authoritative by definition.

### V5 — Re-embedding is delegated to the existing indexing pass

Import writes content and lets `indexing.rs` notice. It compares an entity's `updated_at` against the
newest chunk stored for it, so an imported document whose `updated_at` moved is already stale by that
crate's own rule.

Nothing here calls the embedder. `indexing.rs` already documents why embedding is a background pass
rather than a write hook — a stopped Ollama must not turn importing a file into an error — and routing
import through it inherits that property for free.

### V6 — Watching is a poll, not a filesystem notification API

The importer scans the folder on a schedule. No `FSEvents`, no `inotify`, no `ReadDirectoryChangesW`.

A cross-platform watcher is a dependency and a source of platform-specific missed events, and the
requirement is loose: a document edited now needs to be searchable in minutes, not milliseconds.
AnythingLLM polls at ten-minute intervals on desktop for the same reason. Polling also composes with
`Importer`, which is invoked rather than event-driven.

Directory traversal is bounded — a maximum depth, a maximum file count, an extension allowlist, and a
per-file size cap — because pointing this at a home directory should degrade gracefully instead of
embedding a machine.

---

## Architecture

```
watched folder ──scan──► sources::Documents (SourceConnector)
                                │
                       Spec 1's Importer
                                │
                external_items + documents table
                                │
                     indexing.rs (next pass)  ──► searchable

vault sink refuses write (fingerprint mismatch)
        └─► divergence record ──► UI: keep / overwrite / copy-to-note
```

| Location | Contents | New? |
|---|---|---|
| `connectors/src/sources/documents.rs` | Folder scan as a `SourceConnector` | new |
| `storage/src/migrations.rs` | `documents`, `vault_divergences` | edit |
| `storage/src/repositories/document.rs` | `DocumentRepository` | new |
| `connectors/src/sinks/vault.rs` | Record divergence instead of only refusing | edit |
| `api-server/src/routes.rs` | Folder config, divergence list and resolutions | edit |
| `apps/desktop/src/views/` | Document list, divergence resolution prompt | edit |

### Data model

```sql
CREATE TABLE documents (
    id               TEXT PRIMARY KEY NOT NULL,
    external_item_id TEXT NOT NULL UNIQUE
                     REFERENCES external_items(id) ON DELETE CASCADE,
    path             TEXT NOT NULL,
    title            TEXT NOT NULL,
    body             TEXT NOT NULL,
    byte_size        INTEGER NOT NULL,
    modified_at      TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);

CREATE TABLE vault_divergences (
    id               TEXT PRIMARY KEY NOT NULL,
    external_item_id TEXT NOT NULL UNIQUE
                     REFERENCES external_items(id) ON DELETE CASCADE,
    path             TEXT NOT NULL,
    detected_at      TEXT NOT NULL,
    resolved_at      TEXT,
    resolution       TEXT              -- 'kept'|'overwritten'|'copied_to_note'
);
```

`documents.body` is stored rather than re-read from disk on demand, so search results and grounded
answers survive the file being moved or the drive being unmounted. `updated_at` is what makes
`indexing.rs` re-embed it.

A file that disappears is marked missing rather than deleted, matching how AnythingLLM handles it and
how the vault already handles a file it cannot find — existing embeddings stay, so an answer citing a
document does not become uncitable because someone reorganised a folder.

## Data flow

```
scan (polled)
  └─> walk the folder within depth/count/extension/size bounds
  └─> per file newer than the cursor: Inbound { external_id = stable path hash, … }
  └─> Importer: external_items upsert -> documents upsert (body, modified_at, updated_at)
  └─> cursor = max(modified_at) after the batch commits
  └─> indexing.rs, next pass, re-embeds what moved

vault push
  └─> fingerprint(current file) != remote_version
        ├─> refuse the write            (existing behaviour)
        └─> upsert vault_divergences    (new)
  └─> UI offers keep / overwrite / copy-to-note
        └─> copy-to-note: NoteRepository::create linked to the meeting, then resume mirroring
```

## Error handling

| Condition | Handling |
|---|---|
| Folder unreadable or gone | `Transient`; scan skipped, cursor unmoved, no rows deleted |
| Single file unreadable | Skipped and logged; the batch continues |
| File exceeds the size cap | Skipped with a visible reason, not silently |
| Binary or non-allowlisted extension | Ignored during traversal |
| Traversal hits the depth or count bound | Stops and reports it, rather than truncating silently |
| Vault file unreadable during fingerprint check | Treated as diverged; refusing is the safe default |
| Divergence resolution fails mid-way | Divergence stays unresolved; retryable |

Treating an unreadable vault file as diverged is deliberate: the failure mode of guessing "unchanged"
is overwriting a user's edit, which is the exact bug `b6e9c3f` fixed.

## Testing

All in CI against `tempfile` directories:

- Scan finds new, changed, and unchanged files; unchanged files produce no write.
- Idempotence: two scans with no changes produce no second `external_items` row.
- Cursor advances only after the batch commits; an interrupted scan re-reads.
- Bounds: depth, file count, extension allowlist, per-file size cap all enforced and reported.
- A deleted file is marked missing, its embeddings are untouched, and it remains citable.
- Stable `external_id` across scans for the same path; a moved file is a new item and the old one is
  marked missing.
- Vault divergence: a modified file produces a divergence row and no overwrite; each of the three
  resolutions reaches the right end state; an unreadable file is treated as diverged.
- `copy_to_note` creates a note linked to the meeting and mirroring resumes.

Nothing here needs a model, a network, or a permission, so nothing is `#[ignore]`d.

## What this delivers

1. A `documents` source connector importing a watched folder through Spec 1's `Importer`.
2. Bounded traversal that fails visibly rather than embedding a home directory.
3. Imported documents searchable and citable via the existing indexing pass, with no new embed calls.
4. Vault divergence surfaced with three explicit resolutions, including one that preserves the user's
   writing as a linked note.
5. No parsing of meeting markdown, and no automatic merging anywhere.

## Risks and open questions

- **Depends on Spec 1.** Without `Importer` this spec has no engine.
- **Storing `body` duplicates content already on disk**, which costs database size on a large folder.
  The alternative loses citations when files move, which is worse.
- **Poll interval versus expectation.** A user who edits a document and immediately searches for it
  will not find it, and nothing in the UI currently explains why.
- **The copy-to-note resolution is the least obvious of the three** and the most likely to be
  misunderstood, since it leaves the user with content in two places.
- **Extension allowlists age badly** and will not cover something someone cares about.
