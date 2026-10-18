# Desktop app

> **Status: implemented.** The React frontend runs against the engine's loopback REST API, and
> the Tauri shell in `src-tauri/` starts the engine and shows that UI in a window — see the root
> README for running and installing it. macOS system audio needs the Screen & System Audio
> Recording permission and has not yet been run end to end in a release build.

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
| `src/components/IntelPanel.tsx` | Live clarifying questions, decisions and action items |
| `src/views/SettingsView.tsx` | Backend list + model download |
| `src/views/ChatView.tsx` | Ask questions about one meeting |
| `src/views/AskView.tsx` | Ask the whole workspace, answered from your material with citations |
| `src/views/LibraryView.tsx` | Every meeting, newest first |
| `src/views/AboutView.tsx` | What works and what does not |
| `src/components/TopBar.tsx` | Model / Devices / Language pills, panel toggle, local-or-cloud badge |
| `src/components/MeetingLibrary.tsx` | Meeting list |
| `src/components/TranscriptView.tsx` | Live transcript, grouped by speaker |
| `src/components/RecordDock.tsx` | Floating record control and overflow menu |
| `src/lib/api.ts` | Typed client for the engine |
| `src-tauri/` | Tauri shell: starts the engine, owns the window and the assistant hotkeys. Outside the Cargo workspace on purpose |

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

## Design decisions in the new views

**The questions panel never interrupts.** No toast, no sound, no focus steal — a panel a user
can ignore is one they leave open. Only the two highest-cost ambiguity kinds get colour; if
everything is highlighted, nothing is.

**Settings leads with where audio goes.** The active backend banner is green for local and
amber for remote, and every backend row carries a drive or cloud icon. RAM is shown next to
each model because that is the number deciding whether it runs at all.

**Chat is scoped to one meeting.** Grounding on a single transcript keeps answers checkable
against something the user can read; cross-meeting recall is what search and the graph are for.
The conversation resets when the meeting changes, so answers are never grounded in the wrong
material.

**History is a list, not a month grid.** There is no calendar integration yet, so there is
nothing in the future to show — a grid of empty squares would imply scheduling this cannot do.

**About states the gaps.** A user who finds out system audio does not work by recording a call
and getting half a transcript is worse off than one who was told up front.

## Not built yet

- **System audio outside macOS.** Windows (WASAPI loopback) and Linux (PipeWire) capture are not
  implemented; see [`audio-capture`](../../core/crates/audio-capture/).
- **Recognising a voice across meetings.** Speakers are named per meeting; nothing enrolls or
  matches voice prints yet, which Settings says.
