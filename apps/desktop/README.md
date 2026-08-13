# Desktop app

> **Status: UI implemented and verified.** The React frontend runs against the engine's
> loopback REST API. The **Tauri shell is not built yet** — `src-tauri/` is still a scaffold.

## Run it

Two processes. The engine:

```sh
NOTEWISE_BACKEND=mock cargo run -p notewise-cli -- --ephemeral serve
```

Then the UI:

```sh
cd apps/desktop && npm install && npm run dev
```

Open <http://localhost:1420>. Vite proxies `/health` and `/v1` to `127.0.0.1:47821`, so the
frontend is same-origin and there is no CORS handling on the server side.

`NOTEWISE_BACKEND=mock` lets you drive the whole UI with no model installed. Drop it to use
local Ollama, or set `ANTHROPIC_API_KEY` for BYOK.

## Layout

| Path | Purpose |
|---|---|
| `src/components/Sidebar.tsx` | 52px icon rail; shows capture state from any view |
| `src/components/TopBar.tsx` | Model / Devices / Language pills, panel toggle, local-or-cloud badge |
| `src/components/MeetingList.tsx` | Collapsible meeting list |
| `src/components/TranscriptView.tsx` | Live transcript, grouped by speaker |
| `src/components/RecordDock.tsx` | Floating record control and overflow menu |
| `src/lib/api.ts` | Typed client for the engine |
| `src-tauri/` | **Scaffold.** Tauri shell, not yet built |

## Design decisions

**Red is reserved for recording.** It is the only saturated colour in the interface, so if
something is red, audio is being captured. The rail's record icon shows that state even when
you are on another view.

**The local-or-cloud badge is in the chrome, not in settings.** Where a user's audio goes is
the product's central claim; it should be visible without being trusted.

**Consecutive segments from one speaker are grouped.** A per-segment speaker label on every
line is technically accurate and much harder to read back later.

**Recording state is recovered from the engine, not held only in the UI.** The window can be
reloaded while a meeting is running, and the dock has to come back correct.

**Transcript polling is 1s against loopback.** The engine has no push channel yet; a
WebSocket is not worth the protocol for a same-machine request.

## Not built yet

- **The Tauri shell.** Needs the Tauri CLI and a webview toolchain. `src-tauri/` is excluded
  from the Cargo workspace on purpose so engine CI does not depend on GUI tooling.
- **Live audio.** The record button creates and ends a real meeting, but nothing feeds it
  audio — that needs the OS capture backends, which are blocked on a signed bundle and a TCC
  grant (see [`audio-capture`](../../core/crates/audio-capture/)).
- Calendar, settings, and about views are rail entries only.
