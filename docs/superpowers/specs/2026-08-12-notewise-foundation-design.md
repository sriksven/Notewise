# Notewise — Foundation Design

**Date:** 2026-08-12
**Status:** Approved, in implementation
**Supersedes name:** `loomcore` (placeholder in the original planning docs) → `notewise`

---

## 1. Purpose

Notewise is a local-first meeting intelligence and workspace platform. Meetings are
captured and understood on-device, then become notes, tickets, drafted emails, and
notifications — all linked in one queryable object graph.

The architectural promise is "local or cloud, your choice." This design treats that
as a property enforced by a type boundary (`ai-router`), not a marketing claim.

---

## 2. Scope of this design

The original planning documents describe a five-phase, 14+ month roadmap spanning four
product categories and eight client surfaces. This document covers the **foundation**:
the complete repository structure plus the local engine that every later phase depends on.

Delivery is tiered by what can be *verified* in a headless build environment:

| Tier | Meaning | Contents |
|---|---|---|
| **1** | Compiles, tested, runs | `storage`, `graph`, `ai-router`, `api-server`, `mcp-server`, `cli` |
| **2** | Real interfaces, partial impl | `audio-capture`, `transcription`, `diarization`, `sync-client`, `ffi` |
| **3** | Contract-only scaffolds | `apps/*` (except cli), all of `cloud/` |

Tier 3 is not a shortcut. Those components require Xcode signing, device simulators,
TCC permission grants, and third-party credentials (Stripe, Gmail OAuth, Linear/Jira
API keys) that do not exist in a build environment. Their interfaces are defined so the
architecture is fixed; their implementations wait for their phase.

---

## 3. Deviations from the original plan

Three deliberate changes, each with a reason:

### 3.1 `graph` moves from Phase 1 to Phase 0

The plan calls `graph` "where your core differentiator technically lives," then schedules
it one phase after `storage`. Building `storage` alone first means every entity gets
written without edges, and the graph becomes a migration rather than a foundation.
Retrofitting relationships onto populated tables is the expensive ordering.

### 3.2 `ai-router` ships with a mock backend from the first commit

The plan names this the one boundary worth protecting under pressure to ship. A boundary
is only protected if it is *testable*. Without a mock backend, every test touching
summarization needs a GPU or a paid API key, so tests get skipped and the seam erodes.
`MockBackend` makes the router testable in CI with no external dependency.

### 3.3 Licensing split is wired in at the first commit

MIT for `core/` and `apps/`, BSL-1.1 for `cloud/`. Relicensing after outside
contributions requires tracking down every contributor for permission. Doing it now costs
two files; doing it later can be legally impossible.

---

## 4. Core engine architecture

```
                 ┌──────────────────────────────┐
                 │      graph (object graph)     │
                 │  typed nodes + typed edges    │
                 └──────────────┬───────────────┘
                                │ depends on
                 ┌──────────────▼───────────────┐
                 │   storage (SQLite + repos)    │
                 └──────────────────────────────┘

  ai-router ──── one trait, three backends (mock / ollama / anthropic)
      ▲
      │ every feature calls this; nothing calls a provider directly

  audio-capture → transcription → diarization   (the capture pipeline)

  api-server ─┐
  mcp-server ─┼── surfaces over graph + storage + ai-router
  cli        ─┘
  ffi        ──── C ABI for iOS/Android
```

### 4.1 Dependency rule

Dependencies point **downward only**. `graph` may depend on `storage`; `storage` must
never depend on `graph`. Surfaces (`api-server`, `mcp-server`, `cli`, `ffi`) may depend on
anything below them but never on each other. This keeps a local-only build able to exclude
`sync-client` entirely.

### 4.2 `storage`

Owns the SQLite schema, migrations, and encryption at rest. Exposes repositories
(`MeetingRepository`, `NoteRepository`, …) — never raw SQL to callers outside the crate.
Uses `rusqlite` with the `bundled` feature so no system SQLite is required.

### 4.3 `graph`

Sits on top of `storage`. Nodes are `(NodeKind, Id)` references to entities; edges are
typed (`DerivedFrom`, `References`, `Owns`, `Mentions`, …). Provides traversal — the
`related(node, depth)` query is what makes "everything connected to this meeting" a real
operation instead of a hand-written join per feature.

### 4.4 `ai-router`

One trait with `summarize`, `extract_decisions`, `extract_action_items`, `chat`. Three
interchangeable backends. Callers depend on the trait, never on a provider type. This is
the seam the entire local-or-cloud claim rests on.

---

## 5. Data model

```
Workspace ──has many── Project
Project   ──has many── Meeting, Note, Ticket

Meeting
 ├── has many → TranscriptSegment (speaker, text, start_ms, end_ms)
 ├── has one  → Summary
 └── belongs to → Project (optional)

Summary
 ├── has many → Decision (text, reasoning, timestamp)
 └── has many → ActionItem (owner, due, status) ──may link to→ Ticket

Note      ──references→ Meeting | Ticket | Note   (graph edges)
Ticket    ──may mirror→ external tracker issue
EmailDraft ──generated from→ Meeting + Summary
Notification ──references→ source entity, targets User via channel
```

Relationships marked "references"/"links to" are **graph edges**, not foreign keys. Foreign
keys model ownership (a segment belongs to exactly one meeting); edges model association
(a note may reference many things, discovered by traversal).

---

## 6. Error handling

Each crate defines its own error enum via `thiserror` and returns
`Result<T, CrateError>`. No `unwrap()` in library code. Surfaces (`api-server`, `cli`)
translate errors into transport-appropriate forms — HTTP status codes, process exit codes —
at the boundary, so core crates never encode transport concerns.

---

## 7. Testing

- Unit tests colocated in each crate.
- `storage` and `graph` test against in-memory SQLite (`:memory:`) — fast, no fixtures.
- `ai-router` tests run against `MockBackend`.
- Capture and transcription tests use synthetic audio buffers, not recorded files.
- CI runs `cargo test --workspace` plus `fmt` and `clippy -D warnings`.

Tests requiring a GPU, a signed app bundle, a TCC grant, or a paid API key are marked
`#[ignore]` with a reason, so a green CI run never implies unverified things work.

---

## 8. Known constraints

These are environmental limits, recorded so they are not rediscovered later:

- **macOS system audio** requires ScreenCaptureKit plus a TCC permission grant against a
  signed bundle. Cannot be exercised headlessly; mic capture via `cpal` can.
- **Whisper inference** requires a cmake build of whisper.cpp and a model download
  (150 MB – 1.5 GB). The model registry and download logic are real; inference is behind
  a feature flag.
- **Browser extension** cannot run Whisper at usable speed — it must stream tab audio to
  the desktop app's local `api-server`. This means the desktop app must be running. That
  constraint is stated in the extension's README rather than discovered by users.
- **Wearables do not run the engine.** They are companion surfaces to the phone app.

---

## 9. Open items for the user

- `notewise` is unverified on npm and GitHub; trademark and domain checks remain outstanding.
- BSL-1.1 change-date and change-license for `cloud/` are set to conventional defaults
  (4 years → Apache-2.0) and should be reviewed against commercial intent.
