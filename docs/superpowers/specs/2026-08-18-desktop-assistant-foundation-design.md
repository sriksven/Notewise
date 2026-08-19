# Desktop assistant foundation — design

**Date:** 2026-08-18
**Status:** draft, awaiting review
**Scope:** Spec 9 of the program map. The shared foundation for system-wide surfaces, plus a
staging plan for the four features built on it.

---

## Read this first: scope honesty

This is not one feature. AnythingLLM ships four separate system-wide capabilities — an overlay
assistant, dictation, a highlight-and-act tool, and inline completion — and each is a product in
its own right. Together they are the largest item in the program map by a wide margin, and they
are a **different product from meeting intelligence**: nothing in them involves a meeting.

They also sit in Phase 3-4 territory per `docs/roadmap.md`, whose own gating rule says the
directory structure "is not license to write Phase 3 code during Phase 0."

This spec therefore does two things and deliberately not more:

1. Designs the **shared foundation** all four need — permissions, global hotkeys, text insertion,
   screen context — because building it once correctly is the difference between four features and
   four sets of native bugs.
2. **Stages the four features** as separate specs, so each gets its own design cycle rather than
   being waved at here.

Recommendation: build the foundation and **9a (dictation)** only, then reassess. Dictation is the
one that reuses the most existing machinery and needs the least new permission.

## Why this exists at all

The strategic argument is real. Notewise already has a local STT stack, a local model seam, and a
permissions crate. A user who trusts it with their meetings has already installed the hardest part
of a private, on-device assistant. The marginal cost of dictation over that base is low, and it is
the feature AnythingLLM gates behind Pro — which says something about willingness to pay.

The counter-argument is equally real: it competes for effort with the meeting product that is the
actual thesis, and it multiplies the surface area of the most fragile code in the repo.

## Goals (foundation only)

- One place that owns OS permission state for Accessibility, Input Monitoring, and Screen Recording.
- Global hotkey registration that does not conflict with the host app or with itself.
- Text insertion at the cursor in an arbitrary application, or a clean refusal.
- Screen context capture reduced to text, with OCR fallback for non-vision models.
- All `unsafe` confined to one auditable crate, per the pattern `macos-permissions` establishes.

## Non-goals

- **Any of the four features.** They are staged below, not designed here.
- **Linux.** AnythingLLM's own Linux support for these is degraded or absent, and the underlying
  APIs have no portable equivalent. Foundation returns `Unsupported` with a reason.
- **Cloud-assisted anything.** These surfaces see arbitrary screen content and keystrokes; routing
  that to a remote model is a decision this spec does not make.

---

## Decisions

### A1 — `unsafe` goes in one new crate; `macos-permissions` grows only permission reads

`core/crates/os-input` is the single crate permitted `unsafe` here: hotkey registration, cursor
text insertion, selection reading, and screen capture-to-text. `macos-permissions` gains
Accessibility and Input Monitoring *status* reads, which is what it already exists to do.

This follows the reasoning `macos-permissions` states in its own module docs — the unsafe calls
"live here, in a file short enough to audit in one sitting" — and keeps every other crate on
`forbid(unsafe_code)`.

Behind an `os-input` feature, off by default, exactly as `os-capture` is. Engine CI must not
require an Accessibility grant or a windowing toolchain.

### A2 — Permissions are read, never silently requested, and every feature degrades explicitly

Accessibility and Input Monitoring cannot be granted programmatically; the OS opens a settings
pane and the user toggles a switch, then in many cases the app must restart.

So the foundation exposes status and a *request* that opens the relevant pane, and every feature
built on it must state what it cannot do without the grant rather than appearing broken. This is
the pattern `audio-capture` already uses: `CaptureError::Unsupported { what, reason }` rather than
silently producing nothing, and `63f6f6d fix(capture): stop demanding a permission this build
cannot be given` is the commit that learned this lesson the hard way.

A build without a signed bundle and Team ID cannot hold these grants at all —
`macos_permissions::has_team_identifier` and `can_hold_screen_recording` already encode exactly
this, and the foundation reuses them rather than rediscovering it.

### A3 — Text insertion tries the accessibility API, then the clipboard, then refuses

Three tiers, in order: set the focused element's value through the accessibility API; synthesise a
paste with clipboard save-and-restore; refuse with a reason.

Clipboard paste is a real technique and a real hazard — it clobbers whatever the user had copied,
and restoring it races with anything else reading the clipboard. It is second, not first, and never
silent.

Refusing is a supported outcome. Some applications accept neither, and a feature that inserts text
into the wrong field is worse than one that says it cannot.

### A4 — Screen context is text by the time it leaves the foundation

The foundation returns extracted text, not pixels: window title, focused element text, selection,
and OCR of a captured region when nothing structured is available.

Returning images would push the vision-vs-OCR decision into every consumer and make the privacy
question — what exactly left the machine — depend on which feature was calling. One text-shaped
contract means one answerable question.

Screen capture requires the Screen Recording grant that `macos-permissions` already reads and that
`CLAUDE.md` already warns needs a signed bundle.

### A5 — Hotkeys are registered centrally with conflict detection

One registry owns every global hotkey, checks for conflicts among Notewise's own bindings, and
surfaces a failure when the OS refuses a registration because another app holds it.

Four features registering hotkeys independently produces the bug where enabling one silently breaks
another, and the user has no way to see why.

Defaults avoid the combinations the host OS and common editors claim, and every binding is
user-configurable — a hardcoded hotkey that collides with someone's IDE is an uninstall.

### A6 — Nothing here is verifiable in CI, and the tests must say so

Every native path needs a grant that a `cargo test` binary cannot hold. Per `CLAUDE.md` rule 6,
those tests are `#[ignore]`d with the reason stated.

What *is* testable, and must be separated to stay testable: the hotkey registry's conflict logic,
the insertion tier-selection state machine, permission-state transitions over a mocked provider, and
the text-extraction reduction. The `audio-capture` split — pure logic plus `FileSource` and
`SyntheticSource` real everywhere, native behind a feature — is the model.

---

## Architecture

```
   ┌──────────────── consumers (staged, 9a–9d) ────────────────┐
   │  dictation   overlay   highlight-act   completion         │
   └───────────────────────────┬───────────────────────────────┘
                               ▼
              core/crates/os-input   (feature: os-input)
                ├── hotkeys   registry + conflict detection (pure part testable)
                ├── insert    accessibility → clipboard → refuse
                ├── select    read current selection
                └── context   window/element text, OCR fallback
                               │
                               ▼
              core/crates/macos-permissions  (+ Accessibility, Input Monitoring reads)
```

| Location | Contents | New? |
|---|---|---|
| `core/crates/os-input` | Hotkeys, insertion, selection, screen-context-to-text | **new crate** |
| `core/crates/macos-permissions` | Accessibility + Input Monitoring status and request | edit |
| `apps/desktop/src-tauri` | Overlay window, hotkey wiring | edit (outside workspace) |

`os-input → macos-permissions`. Nothing in the engine depends on `os-input`; only the desktop shell
and the staged features do.

## Staging plan

Each is a separate spec with its own design cycle. Ordered by ratio of value to new risk.

### 9a — Dictation (recommended first, and possibly only)

Global hotkey, capture from the mic, transcribe locally, insert at cursor.

Reuses more than any other: `audio-capture`'s `MicrophoneSource`, the `transcription` crate's
Whisper and Parakeet engines and its model registry, and the microphone permission `macos-permissions` already handles.
The only genuinely new native requirement is insertion (A3).

Two output modes worth distinguishing, following AnythingLLM's split: raw transcription, and a
model-cleaned version that fixes punctuation and grammar. The raw path needs no LLM at all, which
makes it the one feature here that works with no model configured.

### 9b — Overlay assistant

Global hotkey opens a panel that answers questions with the focused application's text as context.

Needs A4's screen context and the Screen Recording grant, therefore a signed bundle. Reuses `ask.rs`
for grounded answering. The hard part is not the model call; it is deciding what context to include
without shipping the user's entire screen to a prompt.

### 9c — Highlight and act

Selection triggers an affordance; a quick action revises, summarises, or translates in place.

Needs A3 insertion and selection reading, and must distinguish editable from non-editable targets so
"replace" is only offered where it can work. Lowest model cost, highest interaction-design cost.

### 9d — Inline completion

Ghost-text suggestions as the user types, accepted with a key.

Needs Input Monitoring — the most invasive grant in the set — plus latency low enough to be useful,
which in practice means a small local model and per-keystroke debouncing. Highest risk, most easily
made annoying, and the one whose value is least established for this product's users. Last, if ever.

## Error handling

`OsInputError`, its own `thiserror` enum:

| Condition | Variant | Behaviour |
|---|---|---|
| Grant absent | `PermissionRequired { what, how_to_grant }` | Feature disabled with an actionable message |
| Build cannot hold the grant | `Unsupported { what, reason }` | Same shape `audio-capture` already returns |
| Hotkey already held | `HotkeyUnavailable { binding }` | Surfaced at bind time, not at press time |
| Two Notewise bindings collide | `HotkeyConflict { a, b }` | Rejected at configuration time |
| Insertion impossible | `InsertionRefused { reason }` | Text offered on the clipboard instead, said out loud |
| Platform unsupported | `Unsupported` | Linux, always |

`PermissionRequired` carries how to grant it. A permission error that does not tell the user which
pane to open is a dead end, and this is the class of bug `63f6f6d` was about.

## Testing

Runs in CI:

- Hotkey registry: conflict detection among own bindings, rejection of duplicates, default set is
  internally conflict-free.
- Insertion tier selection over a mocked backend: accessibility succeeds; accessibility fails and
  clipboard succeeds; both fail and it refuses; clipboard contents restored after a synthesised
  paste.
- Permission state machine over a mocked provider, including the not-signed-bundle case that
  `has_team_identifier` already detects.
- Screen-context reduction: structured text preferred over OCR; empty context is a valid result, not
  an error.
- Every `Unsupported` path on Linux.

`#[ignore]`d with reasons: anything needing a real grant, a real hotkey press, a real insertion into
a third-party app, or a signed bundle.

## What this delivers

1. `core/crates/os-input` behind an off-by-default feature, with all `unsafe` in it.
2. Accessibility and Input Monitoring status reads in `macos-permissions`.
3. A central hotkey registry with conflict detection and configurable bindings.
4. Three-tier text insertion that never silently does the wrong thing.
5. Screen context reduced to text at the boundary.
6. Four staged specs, each with a stated dependency on this foundation.

## Risks and open questions

- **This spec's real risk is opportunity cost.** It is the largest item in the program map and the
  least connected to meeting intelligence. Building the foundation is defensible; building all four
  features before the meeting product is complete probably is not.
- **Unverifiable in CI, permanently.** Every native path arrives `#[ignore]`d, which means the green
  build says less than it does anywhere else in the repo.
- **Accessibility and Input Monitoring are the two most alarming grants on macOS**, and asking for
  them is a trust event for a product sold on privacy. The request UX matters more than the feature.
- **A signed bundle with a Team ID is required** for screen context, so 9b cannot be developed
  against an ad-hoc build — the same wall `CLAUDE.md` documents for system audio.
- **Insertion via synthesised paste is inherently racy** with clipboard managers, which a meaningful
  number of developers run.
