# Architecture

How Notewise is put together, and why the boundaries sit where they do.

---

## The shape of it

One Rust engine. Thin platform shells around it. Everything else is a surface over the
same core.

```
┌──────────────────────────────────────────────────────────────┐
│  Surfaces:  desktop · mobile · wearable · extension · CLI     │
│             api-server (REST) · mcp-server (agents) · ffi     │
└───────────────────────────┬──────────────────────────────────┘
                            │
┌───────────────────────────▼──────────────────────────────────┐
│  graph        typed nodes + typed edges, traversal            │
├──────────────────────────────────────────────────────────────┤
│  storage      SQLite, migrations, repositories, encryption    │
└──────────────────────────────────────────────────────────────┘

  ai-router          one trait, three backends — the local/cloud seam
  capture pipeline   audio-capture → transcription → diarization
  sync-client        inert unless the user opts in
```

## The dependency rule

Dependencies point **downward only**.

- `graph` may depend on `storage`. `storage` must never depend on `graph`.
- Surfaces may depend on anything beneath them, but **never on each other**.
- Nothing above `ai-router` may reference a model provider type directly.

The payoff is that a local-only build can exclude `sync-client` from compilation
entirely, and swapping a transcription engine does not ripple upward.

---

## Crate responsibilities

### `storage` — the only thing that writes SQL

Owns schema, migrations, and encryption at rest. Exposes repositories
(`MeetingRepository`, `NoteRepository`, …). Callers outside this crate never see SQL.

Uses `rusqlite` with `bundled`, so there is no system SQLite dependency and builds are
reproducible across platforms.

### `graph` — why this exists as its own layer

Foreign keys model **ownership**: a transcript segment belongs to exactly one meeting.
That is a `storage` concern.

Edges model **association**: a note references a meeting, which produced a decision, which
became a ticket, which is mentioned in an email. Those relationships are many-to-many,
heterogeneous, and traversable to arbitrary depth.

Encoding association as foreign keys means a bespoke join for every new relationship type
and a schema migration every time the product grows a link. The graph makes
`related(node, depth)` a single operation. This is where the product differentiator
technically lives, which is why it is a layer rather than a helper module.

### `ai-router` — the seam worth protecting

One trait: `summarize`, `extract_decisions`, `extract_action_items`, `chat`.
Three backends: `MockBackend`, `OllamaBackend`, `AnthropicBackend`.

Every feature calls the trait. Nothing calls a provider directly. This is what makes
"local or cloud, your choice" an architectural property rather than a marketing claim —
it is checked by the compiler, not by discipline.

`MockBackend` exists from the first commit on purpose. A boundary is only protected if it
is testable; without a mock, every test touching summarization would need a GPU or a paid
API key, so tests get skipped and the seam quietly erodes.

### Capture pipeline

`audio-capture` defines one interface — give me a stream of audio frames — with per-OS
implementations underneath (WASAPI loopback, Core Audio / ScreenCaptureKit, PipeWire).
Nothing above it knows which OS it runs on.

`transcription` consumes frames and emits timestamped segments. Whisper.cpp and Parakeet
sit behind one interface, so swapping engines is configuration.

`diarization` is deliberately **not** folded into transcription. Speaker separation quality
needs to be iterated on independently, and users need to be able to turn it off, without
touching the transcription path.

### Surfaces

`api-server` binds to localhost only. `mcp-server` exposes the same operations as MCP
tools for agents. `ffi` exports a stable C ABI so Swift and Kotlin link the compiled core
rather than reimplementing it per platform.

---

## Data model

Ownership is foreign keys. Association is edges.

```
Workspace ──has many── Project ──has many── Meeting · Note · Ticket

Meeting ──┬── TranscriptSegment (speaker, text, start_ms, end_ms)
          └── Summary ──┬── Decision
                        └── ActionItem ──may link to──> Ticket

Note ──references──> Meeting | Ticket | Note        (edges)
EmailDraft ──generated from──> Meeting + Summary    (edges)
Notification ──references──> any source entity      (edges)
```

---

## Error handling

Each crate defines its own error enum with `thiserror`. No `unwrap()` in library code.

Surfaces translate errors at the boundary — `api-server` maps to HTTP status codes, `cli`
maps to exit codes. Core crates never encode transport concerns, so the same error type
serves REST, MCP, FFI, and the CLI without modification.

---

## Testing

`storage` and `graph` test against in-memory SQLite, so tests are fast and need no
fixtures. `ai-router` tests run against `MockBackend`. Capture tests use synthetic buffers
rather than recorded audio.

Tests that need a GPU, a signed bundle, a TCC grant, or a paid API key are marked
`#[ignore]` **with a stated reason**. A green CI run must never imply that unverified
things work.

---

## Versioning

`core/` is semver-versioned independently of the apps that consume it, because external
integrators depend on the MCP server and local API staying stable. Platform releases are
tagged separately — a desktop patch should not force a mobile version bump.

Trunk-based development, short-lived branches, conventional commits.

---

## Known constraints

Recorded here so they are not rediscovered painfully later.

- **macOS system audio** needs ScreenCaptureKit plus a TCC grant against a signed bundle.
  Cannot be exercised headlessly.
- **The browser extension cannot run Whisper** at usable speed. It streams tab audio to the
  desktop app's local `api-server`, which means the desktop app must be running. This is
  stated plainly in the extension README rather than left for users to discover.
- **Wearables do not run the engine.** They are companion surfaces to the phone app — a
  remote control and a glanceable display, not a port target.
- **Two-way ticket sync is a known hard problem.** Phase 2 ships one-way push only;
  two-way lands after that is stable.
