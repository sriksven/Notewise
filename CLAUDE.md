# Notewise — guide for AI coding agents

Orientation for agents working in this repo. Read this before making changes.

## What this is

Local-first meeting intelligence. A Rust engine (`core/`) with thin platform shells
(`apps/`) and opt-in hosted services (`cloud/`). Meetings become notes, tickets, emails,
and notifications linked in one object graph.

## Build and test

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

`apps/desktop/src-tauri` is excluded from the workspace on purpose — it pulls a GUI
toolchain that engine CI should not depend on. Build it separately if you are working on
the desktop app.

## Rules that matter here

**1. Never call a model provider directly.** Everything goes through the `ai-router`
trait. If you find yourself importing `reqwest` to hit an LLM API from outside
`core/crates/ai-router`, stop — you are breaking the seam the entire local-or-cloud
promise depends on.

**2. Dependencies point downward only.** `graph` may depend on `storage`; `storage` must
never depend on `graph`. Surfaces (`api-server`, `mcp-server`, `cli`, `ffi`) never depend
on each other.

**3. SQL lives only in `storage`.** Other crates use repositories. If you need a new query,
add a repository method rather than reaching for a connection.

**4. Ownership is foreign keys; association is edges.** A transcript segment belongs to one
meeting — foreign key. A note references a meeting — graph edge. Getting this wrong means a
schema migration every time the product grows a link type.

**5. No `unwrap()` in library code.** Each crate has a `thiserror` enum. Surfaces translate
to HTTP codes or exit codes at the boundary.

**6. Mark unverifiable tests `#[ignore]` with a reason.** Anything needing a GPU, signed
bundle, TCC grant, or paid API key. A green CI run must never imply unverified things work.

## Licensing — check before you add code

| Path | License |
|---|---|
| `core/`, `apps/` | MIT |
| `cloud/` | BSL 1.1 |

Moving code from `cloud/` into `core/` relicenses it. Do not do this incidentally.

## State of the repo

Many directories are scaffolds awaiting their phase. **Every scaffold's `README.md` states
its status at the top.** Read it before assuming something is implemented. Do not write
Phase 3 features during Phase 0 — see [ROADMAP.md](ROADMAP.md).

## Conventions

Conventional commits (`feat:`, `fix:`, `docs:`, `chore:`). Trunk-based, short-lived
branches off `main`. `core/` is semver-versioned independently of the apps consuming it.

## Things that will bite you

- **macOS system audio** needs ScreenCaptureKit + a TCC grant against a signed bundle. It
  cannot be tested headlessly. Mic capture via `cpal` can.
- **Whisper inference** needs a cmake build of whisper.cpp and a model download. It sits
  behind a feature flag so the default build stays fast.
- **The browser extension cannot run Whisper.** It streams tab audio to the desktop app's
  local `api-server`. The desktop app must be running.
- **`rusqlite` uses `bundled`.** Do not switch to system SQLite; it breaks reproducibility
  across platforms.
