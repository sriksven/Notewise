# Browser extension

> **Status: implemented, unverified against live meeting pages.** The platform-independent logic is
> tested (`npm test`, 13 tests). The DOM selectors are not, and cannot be — see
> [What is not tested](#what-is-not-tested).

Tells your local Notewise engine **who is speaking**. It sends names and speaking times. It does not
send audio.

## The problem it solves

On a five-person call, macOS system audio capture gives the engine one channel carrying four people.
Every word from the far end is correctly labelled `Others`, and that is as far as the audio can take
it — clustering voices can prove there were four of them, but nothing in an audio signal says one of
them is called Priya.

The meeting platform knows. Meet, Zoom, and Teams route the audio, so they know exactly who is
unmuted, and all three show it on screen. This extension reads that and posts it to the engine, which
turns `Others` into four names.

## Why this replaced the audio-streaming design

This directory previously specified `chrome.tabCapture` streaming tab audio to the desktop app. That
design had a real problem — the extension cannot run Whisper, so it needed the desktop app as an
audio sink, which was documented as a product limitation to be resolved later.

The limitation dissolves once you notice the engine **already has the audio**. The only thing missing
was identity, and identity is a few hundred bytes a minute. Separating the identity channel from the
audio channel removed the whole problem: no media streaming, no `tabCapture` permission, no
duplicated capture path.

The same reasoning rules out the other obvious approach. A bot that joins the meeting by link would
acquire an audio stream the engine already has, in order to obtain identity — paying for the
expensive half to get the cheap half, plus a headless browser, anti-automation countermeasures, and a
fifth participant in the call. See `docs/superpowers/specs/2026-08-14-speaker-identity-design.md`.

## What it sends

| Sent | Not sent |
|---|---|
| Participant display names | Audio |
| Time spans of who was speaking | Video |
| Which participant is you | Chat, screen contents, page text |
| | Meeting title or URL |

To `http://127.0.0.1:47821/v1/meetings/:id/speaker-events`, the same loopback API the CLI and the
desktop frontend use. The extension gets no special access.

**It runs only while the engine is recording.** With no recording in progress, no timer runs and
nothing is read. That is a privacy property, not an optimisation — the extension is inert unless you
have already chosen to record.

## Permissions

`"permissions": []`. Empty, and worth stating plainly: no `tabCapture`, no `storage`, no `scripting`,
no `activeTab`. The only `host_permissions` entry is `http://127.0.0.1:47821/*`.

## Install

Not packaged yet. Load unpacked:

1. Start the desktop app, or `notewise serve`.
2. `chrome://extensions` → Developer mode → **Load unpacked** → select this directory.
3. Start a recording, then join a meeting.

There is no build step. The files that ship are the files in `src/`.

```sh
npm test    # node --test test/
```

## Design

Manifest V3. `src/content.js` is a loader — a content script is not an ES module and cannot
`import`, so it reaches `src/session.js` by dynamic import. Keeping the logic in real modules is what
makes it testable under `node --test` with no bundler to drift out of date.

| File | Job |
|---|---|
| `src/platforms.js` | Reduces each platform's DOM to `{id, displayName, speaking, isLocal}[]` |
| `src/tracker.js` | Turns polled observations into turns on the recording's clock |
| `src/engine.js` | The loopback API client |
| `src/session.js` | Starts and stops tracking with the engine's recording |

### Two things that are subtle

**The clock.** The engine wants milliseconds since recording start; the page knows
`performance.now()`. Those origins are unrelated and the offset is unrecoverable afterwards, so it is
fixed once when tracking starts.

**A poll is not an event.** No platform offers a "speaker changed" callback, so this samples every
250 ms. Turn edges are therefore only as precise as the sampling interval, which is why the engine
treats these boundaries as weaker evidence than acoustic ones and prefers `NamedClusterDiarizer` —
acoustic boundaries, platform names.

## What is not tested

**The DOM selectors.** They assert against markup owned by Google, Zoom, and Microsoft. A fixture
test would only prove the fixture matches itself, and would keep passing after a vendor redesign
broke the real thing — a green suite implying working attribution is worse than no test.

So the selectors are built to fail loudly instead:

- Each field has several selectors, tried in order. `aria-*` and `data-*` attributes come first
  because assistive technology depends on them, so vendors change them rarely. Generated class names
  are deliberately absent — they change on every deploy.
- When the roster cannot be read at all, the adapter returns `null`, which closes every open turn
  rather than assuming the last speaker continued.
- After about thirty seconds of unreadable samples, tracking **stops** and logs that the markup has
  probably changed. Speakers fall back to anonymous acoustic labels.

That last rule is the important one. A transcript labelled `Speaker 2` is a mild disappointment. A
transcript that attributes words to a named colleague who did not say them is a serious defect, and
nothing downstream can tell it from a correct one. Silence degrades cleanly; a wrong name does not
degrade at all.

## Phase

Ahead of its phase. `docs/roadmap.md` puts this directory in Phase 3, and the engine work it depends
on is Phase 1. It is here because it is the cheapest route to the named speakers the diarization work
was built for, and because it needs no cloud service, no vendor account, and no bot.
