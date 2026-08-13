# Browser extension

> **Status: scaffold.** No implementation yet — this directory exists so the architecture does not need reshaping when Phase 3 arrives. See [ROADMAP.md](../../ROADMAP.md).

Captures tab audio for browser-based meetings and streams it to the desktop app.

## The constraint, stated plainly

**The extension cannot run Whisper itself** at usable speed. It streams captured tab audio to
the desktop app's local API server, which means **the desktop app must be running** for the
extension to work.

That is a real product limitation, not an implementation detail. It is documented here rather
than left for users to discover, and it needs a deliberate decision before Phase 3: either
state it plainly in the onboarding, or route extension-only users through cloud processing
instead.

## Design

Manifest V3. `chrome.tabCapture` for audio, streamed to `http://127.0.0.1:47821` — the same
loopback-bound API the CLI serves. The extension is just another local API client; it gets no
special access.
