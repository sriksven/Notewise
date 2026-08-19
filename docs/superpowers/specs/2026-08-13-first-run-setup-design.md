# Notewise — First-Run Setup Design

**Date:** 2026-08-13
**Status:** Approved, awaiting implementation
**Scope:** `apps/desktop`, `core/crates/{api-server,storage,audio-capture,ai-router}`

---

## 1. Purpose

A first launch of the desktop app currently drops the user into the main window with no
transcription model on disk, no confirmed AI backend, and no microphone grant. Nothing
announces this. The first recording attempt fails, or worse, appears to succeed and
produces an empty transcript.

This design adds a first-run wizard that resolves all three before the app opens, and a
way for the app to notice afterwards when one of them regresses.

---

## 2. Requirements

Settled during brainstorming; recorded here because each one excludes a simpler design.

| # | Requirement |
|---|---|
| R1 | Four steps: Welcome → Model → Backend → Permissions |
| R2 | Every step is **required**. There is no "skip for now" |
| R3 | A requirement that is *impossible to satisfy on this build* does not block |
| R4 | The wizard does not download an LLM. It detects the configured backend and reports it |
| R5 | Setup state survives an app restart |
| R6 | After completion, a regression surfaces as a banner, not a repeat of the wizard |

### 2.1 R3 in detail

Microphone capture needs only the OS microphone permission and is obtainable everywhere
`cpal` runs. It is **always required**.

macOS system audio needs a ScreenCaptureKit grant against a *signed bundle*. In an unsigned
development build that grant cannot be obtained at all, so a strict gate would trap a
developer in the wizard on their own machine. System audio is therefore **required only when
`OsBackend::for_host(CaptureKind::SystemAudio)` reports a usable backend**; otherwise the row
renders the string from `unavailable_reason()` and is excluded from the gate.

This is the whole of R3. There is no environment-variable bypass, and no build in which a
user can dismiss a requirement that is genuinely satisfiable.

---

## 3. Prior art: Meetily

[Zackriya-Solutions/meetily](https://github.com/Zackriya-Solutions/meetily) ships the flow
this design is modelled on: Welcome → Setup Overview → Downloads → Permissions, persisted to
a Tauri store and coordinated by `OnboardingContext.tsx`. Three of its decisions are
deliberately not reproduced.

**Its permission checks do not check permissions.** `usePermissionCheck.ts` derives
microphone access from `inputDevices.length > 0`; devices enumerate fine with the grant
denied. `check_screen_recording_permission()` has the body `true`. `trigger_system_audio_permission`
carries the comment *"even with permission denied, tap creation may succeed but audio will be
silence."* The Permissions screen can show two green checks on a machine that records nothing
— the exact failure the screen exists to prevent. §6 specifies probes that touch the thing
being probed.

**Its completion flag outranks reality.** `"Trust the completed status - don't revert based on
model downloads"`. Delete the models directory and the app still reports setup complete. §4
keeps the flag authoritative for *first-run vs not* only, and derives everything else live.

**Persisting `current_step` created its races.** The context saves the step on every change,
which fights the completion write; hence `isCompletingRef`, `"Skipping saveOnboardingStatus
because completion is in progress"`, and a `currentStep > 4 → 3` clamp. §4 persists no step
index, so there is no race to guard.

---

## 4. State model

### 4.1 One persisted value

```
onboarding_completed_at : RFC 3339 timestamp, absent until Finish
```

Everything else is derived per launch from live readiness. Model downloads are already
engine-owned and recoverable through `GET /v1/downloads`, so partial progress needs no
separate record.

Welcome is the exception, and deliberately so. It has no satisfaction criterion, so "land on
the first unsatisfied step" would skip it entirely. The rule is: **while setup is incomplete
the wizard always opens on Welcome, and Next from Welcome jumps to the first unsatisfied
step.** A user who quits mid-download therefore sees Welcome once more on relaunch and is one
click from where they left off. Tracking "has seen Welcome" would mean persisting a step
index, which is the decision that produced Meetily's races (§3) — a redundant click is the
cheaper trade.

No step index is stored. No per-model status is stored. There is no auto-save effect, so
nothing can overwrite the completion write.

### 4.2 Why the engine and not the shell

`apps/desktop/src-tauri/src/main.rs:83` binds the engine with `Server::bind("127.0.0.1:0")`.
Port 0 means the OS assigns a free port per launch, so the window's origin changes every
launch and `localStorage`, `sessionStorage`, and IndexedDB are all effectively cleared. Any
shell-side store is also a second source of truth alongside the database. State goes in the
database.

### 4.3 Storage

New migration appending to `MIGRATIONS` in `core/crates/storage/src/migrations.rs`:

```sql
CREATE TABLE app_settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

A generic key/value table rather than an `onboarding` table: this is app-level configuration,
not a domain object, and it has no edges. Accessed through a new
`repositories/setting.rs` — `get(key)`, `set(key, value)`, `delete(key)` — because SQL lives
only in `storage` (rule 3).

`SUPPORTED_VERSION` derives from `MIGRATIONS.len()`, so it advances on its own.

---

## 5. API surface

Three new routes in `core/crates/api-server/src/routes.rs`.

### 5.1 `GET /v1/setup`

The readiness snapshot. The engine decides policy; the frontend renders.

```jsonc
{
  "completed_at": null,
  "steps": {
    "model": {
      "satisfied": false,
      "required": true,
      "recommended": "large-v3-turbo",
      "installed": [],
      "directory": "/Users/…/models"
    },
    "backend": {
      "satisfied": true,
      "required": true,
      "kind": "ollama",
      "model": "llama3.2",
      "is_local": true,
      "reachable": true,
      "detail": null
    },
    "permissions": {
      "satisfied": false,
      "required": true,
      "microphone":   { "status": "not_requested", "required": true,  "detail": null },
      "system_audio": { "status": "unavailable",   "required": false,
                        "detail": "ScreenCaptureKit requires a screen-recording permission grant against a signed bundle" }
    }
  }
}
```

`status` is one of `not_requested | granted | denied | unavailable`. `not_requested` is
stated as such rather than guessed — nothing prompts on mount.

This route never prompts. It calls `permissions::status`, never `permissions::request`; a
`GET` that raised a TCC dialog would fire before the user pressed anything. The backend probe
it does run is bounded by a 2-second timeout, so a machine with Ollama installed but not
running returns `reachable: false` promptly instead of stalling the wizard on a connect
timeout.

### 5.2 `POST /v1/setup/complete`

Writes `onboarding_completed_at`. Rejects with `409` and the unsatisfied step names if any
required step is unsatisfied, so the gate cannot be bypassed by calling the API directly.
Idempotent: completing twice leaves the first timestamp in place.

### 5.3 `POST /v1/permissions/:kind`

`kind` ∈ `microphone | system_audio`. Runs the probe in §6 and returns the same per-kind
object as `GET /v1/setup`. This is the "Enable" button's request.

Blocking work runs on `spawn_blocking` — opening an audio device is not async.

---

## 6. Permission probing

New `permissions` module in `core/crates/audio-capture`. It belongs there: `CaptureKind`,
`OsBackend`, and `unavailable_reason()` already live in that crate's `lib.rs`.

```rust
pub enum PermissionStatus { NotRequested, Granted, Denied, Unavailable(String) }

pub fn status(kind: CaptureKind) -> PermissionStatus;   // no prompt
pub fn request(kind: CaptureKind) -> PermissionStatus;  // may prompt
```

**Microphone.** `request` opens the default input stream through `cpal` and drops it
immediately. That is what triggers the TCC prompt on first call and what returns a permission
error when denied — `microphone.rs:292-294` already performs exactly this error-string
mapping, which moves into the new module so both callers share one definition. `status`
returns `NotRequested` until a probe has run; it does not open a device, because doing so on
mount would prompt the user before they pressed anything.

**System audio.** Resolved from `OsBackend::for_host(CaptureKind::SystemAudio)`. `None`, or a
backend whose `unavailable_reason()` is `Some`, yields `Unavailable(reason)`. When a usable
backend exists, `request` attempts to open it and maps the outcome as above.

At the time of writing `unavailable_reason()` returns `Some` for **every** variant — the
non-ScreenCaptureKit arm answers `"this backend is not implemented yet"`. System audio is
therefore never required on any build today, and R3's gate is exercised from the first commit
rather than being dead code awaiting a signed bundle. That is the correct behaviour, not a
gap: the app genuinely cannot capture system audio yet, so requiring the grant would block
every user on a capability that does not exist.

**Feature gating.** `notewise-audio-capture` is an *optional* dependency of `api-server`,
enabled by its `record` feature (`api-server/Cargo.toml`), and `os-capture` is what pulls
`cpal`. A build without `record` has no probe to run, so both kinds report
`Unavailable("built without the 'record' feature")` and neither is required. This mirrors how
`list_devices` (`routes.rs:136-165`) already handles the same split.

No function returns a hardcoded `true`.

---

## 7. Backend detection

`AiBackend` in `core/crates/ai-router/src/lib.rs` gains:

```rust
async fn probe(&self) -> Result<()> { Ok(()) }
```

The default suits every cloud backend: a configured API key is the check, and issuing a real
completion to test reachability would cost money and latency on every launch. Ollama
overrides it with a `GET {endpoint}/api/tags` under a 2-second timeout.

The HTTP call lives inside `ai-router` (rule 1). No other crate gains a provider dependency.

A backend is `satisfied` when a kind and model are resolved and `probe()` succeeds. An
unreachable Ollama is reported with `reachable: false` and a `detail` string; the Backend step
then offers the same two choices it always offers — local, or bring your own key — with the
failure stated rather than hidden.

---

## 8. Frontend

### 8.1 Structure

```
src/onboarding/
  SetupGate.tsx          — renders the flow or the children
  SetupFlow.tsx          — step routing from derived readiness
  Stepper.tsx
  steps/WelcomeStep.tsx
  steps/ModelStep.tsx
  steps/BackendStep.tsx
  steps/PermissionsStep.tsx
  steps/PermissionRow.tsx
  readiness.ts           — pure: snapshot → step list, current step, can-finish
```

`App.tsx` is 409 lines and already carries recording, transcript polling, questions,
summarize, import, and export. It gains a wrapper and nothing else:

```tsx
<SetupGate>
  {/* existing tree, unchanged */}
</SetupGate>
```

`readiness.ts` is pure and holds all the branching — which steps exist on this machine, which
are required, which is current, whether Finish enables. It is the only piece with logic worth
testing directly, and it has no React or fetch dependency.

### 8.2 The four screens

Centred column, existing Tailwind tokens, `lucide-react` icons — consistent with
`SettingsView`. A stepper across the top from step 2 onward.

1. **Welcome.** Product name, one line, three claims (stays on device / meetings become notes,
   tickets, email / works offline). One primary button.
2. **Model.** The recommended model with size and RAM, a progress bar during download showing
   transferred bytes, total, rate, and percent. Reuses the engine's existing SSE stream.
3. **Backend.** Two cards — the detected local backend with a reachability dot, and bring your
   own key with the environment variables to set. Choosing is the requirement; downloading is
   not offered, because the app does not install Ollama.
4. **Permissions.** One row per capability: icon, name, one-line rationale, and an Enable
   button. Granted collapses to a check. Denied offers "Open System Settings" plus a re-check.
   Unavailable shows the reason and is visibly not blocking.

Finish is disabled until every required step is satisfied, and calls `POST /v1/setup/complete`.

### 8.3 After completion

`SetupGate` fetches `GET /v1/setup` once on mount; it does not poll. When `completed_at` is
set but a required step has regressed, the main app shows a dismissible banner naming the
specific problem and linking to the matching Settings section. The wizard does not reappear — a restarted Ollama
must not demote an established user to a welcome screen (R6).

---

## 9. Changes to existing code

Three, each in code this feature touches.

**`routes.rs:1012` resolves the wrong model directory.** `list_models` calls the free
`model_store()`, which reads `NOTEWISE_MODEL_DIR`/`NOTEWISE_DATA_DIR` and otherwise guesses a
platform path. `download_model` at `routes.rs:1052` and `1084` uses `state.model_store()`,
which honours `AppState::with_model_dir`. The desktop shell sets that to
`<app_data>/models` (`main.rs:138`), a different path from the guess. So `/v1/models` can
report `installed: false` for a model that is on disk, which would leave the Model step's gate
permanently unsatisfied. `list_models` moves to `state.model_store()`, and the readiness
endpoint uses the same accessor.

**Duplicate byte formatting.** `size()` in `SettingsView.tsx:7-11` is needed verbatim by
`ModelStep`. It moves to `src/lib/format.ts`.

**Duplicate download orchestration.** `SettingsView.tsx:39-103` holds both the start-download
flow and the effect that re-attaches to a download already running. `ModelStep` needs the
same behaviour. It becomes `src/lib/useModelDownload.ts`, and `SettingsView` switches to it.

No unrelated refactoring.

---

## 10. Testing

### 10.1 Rust — all headless

| Area | Test |
|---|---|
| `storage` | `app_settings` round-trip; overwrite; absent key; migration advances `SUPPORTED_VERSION` |
| `api-server` | `GET /v1/setup` shape; `complete` writes the timestamp; `complete` is idempotent; `complete` returns 409 when a required step is unsatisfied |
| `api-server` | `list_models` honours `with_model_dir` — a regression test for §9 |
| `audio-capture` | `Unavailable` carries the reason when no backend exists; permission-error strings map to `Denied` |
| `ai-router` | default `probe()` returns `Ok`; Ollama `probe()` maps a refused connection to an error |

Tests needing a real device or a real TCC grant are `#[ignore]` with a reason (rule 6).

### 10.2 Frontend

Vitest, added as a devDependency, covering `readiness.ts` only: step list with and without an
available system-audio backend; Next-from-Welcome landing on the first unsatisfied step
(§4.1); Finish gating; the regression case where `completed_at` is set and a required step is
unsatisfied.

The step components are not unit-tested. There is no existing component-test infrastructure,
and adding one is a larger decision than this feature should make.

---

## 11. Out of scope

- Storing API keys. Keys stay in the engine's environment (`api.ts:320-325`); the Backend step
  names the variables and does not collect secrets over HTTP, even on loopback.
- Installing or updating Ollama.
- Re-running the wizard on demand. A "reset setup" control is a Settings feature.
- Windows and Linux permission models beyond what the microphone probe already covers.
- Choosing a non-recommended transcription model during setup. Settings already offers the
  full registry.
