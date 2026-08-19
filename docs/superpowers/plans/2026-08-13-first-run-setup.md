# First-Run Setup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Gate the desktop app's first launch behind a four-step wizard (Welcome → Model → Backend → Permissions) that will not let a user reach the main window without a transcription model on disk, a resolved AI backend, and every obtainable audio permission granted.

**Architecture:** All setup state lives in the engine, because the Tauri shell binds `127.0.0.1:0` and the window's origin therefore changes every launch, wiping browser storage. Exactly one value is persisted — `onboarding_completed_at` in a new `app_settings` table — and every other decision (which step is current, whether Finish enables, whether a requirement applies to this build) is derived per launch from a live readiness snapshot served by `GET /v1/setup`.

**Tech Stack:** Rust (axum, rusqlite, tokio, thiserror, async-trait), React 18 + TypeScript + Tailwind + lucide-react, Vitest.

**Spec:** `docs/superpowers/specs/2026-08-13-first-run-setup-design.md`

---

## File Structure

### Rust — created

| File | Responsibility |
|---|---|
| `core/crates/storage/src/repositories/setting.rs` | `SettingsRepository` — get/set/delete on `app_settings`. The only SQL for it. |
| `core/crates/audio-capture/src/permissions.rs` | `PermissionStatus`, `status()`, `request()`. Probes that touch the device. |
| `core/crates/api-server/src/setup.rs` | Readiness snapshot type + the policy deciding what is required on this build. No HTTP. |

### Rust — modified

| File | Change |
|---|---|
| `core/crates/storage/src/migrations.rs` | Append migration creating `app_settings` |
| `core/crates/storage/src/repositories/mod.rs` | Declare and re-export `setting` |
| `core/crates/audio-capture/src/lib.rs` | Declare and re-export `permissions` |
| `core/crates/audio-capture/src/microphone.rs` | Extract permission-error mapping so both callers share one definition |
| `core/crates/ai-router/src/lib.rs` | `AiBackend::probe()` with a default body |
| `core/crates/ai-router/src/backends/ollama.rs` | Override `probe()` with `GET /api/tags` |
| `core/crates/ai-router/src/router.rs` | `Router::probe()` delegating to the boxed backend |
| `core/crates/api-server/src/lib.rs` | Declare `setup` module |
| `core/crates/api-server/src/routes.rs` | Three routes, three handlers, and the `list_models` model-dir fix |

### Frontend — created

| File | Responsibility |
|---|---|
| `apps/desktop/src/lib/format.ts` | `size()` byte formatter, currently duplicated |
| `apps/desktop/src/lib/useModelDownload.ts` | Start + resume a model download; the SSE lifecycle |
| `apps/desktop/src/onboarding/readiness.ts` | **Pure.** Snapshot → step list, current step, can-finish. All branching lives here. |
| `apps/desktop/src/onboarding/readiness.test.ts` | Vitest unit tests for the above |
| `apps/desktop/src/onboarding/SetupGate.tsx` | Fetches readiness on mount; renders flow, children, or banner |
| `apps/desktop/src/onboarding/SetupFlow.tsx` | Step routing and navigation |
| `apps/desktop/src/onboarding/Stepper.tsx` | The dots-and-connectors header |
| `apps/desktop/src/onboarding/steps/WelcomeStep.tsx` | Screen 1 |
| `apps/desktop/src/onboarding/steps/ModelStep.tsx` | Screen 2 |
| `apps/desktop/src/onboarding/steps/BackendStep.tsx` | Screen 3 |
| `apps/desktop/src/onboarding/steps/PermissionsStep.tsx` | Screen 4 |
| `apps/desktop/src/onboarding/steps/PermissionRow.tsx` | One capability row |
| `apps/desktop/src/onboarding/SetupBanner.tsx` | Post-completion regression notice |

### Frontend — modified

| File | Change |
|---|---|
| `apps/desktop/package.json` | Add `vitest`, add `test` script |
| `apps/desktop/src/lib/api.ts` | Setup types + `setup()`, `completeSetup()`, `requestPermission()` |
| `apps/desktop/src/views/SettingsView.tsx` | Use the extracted `size()` and `useModelDownload` |
| `apps/desktop/src/App.tsx` | Wrap the tree in `<SetupGate>` |

---

## Task 1: `app_settings` table and `SettingsRepository`

**Files:**
- Modify: `core/crates/storage/src/migrations.rs` (append to `MIGRATIONS`)
- Create: `core/crates/storage/src/repositories/setting.rs`
- Modify: `core/crates/storage/src/repositories/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `core/crates/storage/src/repositories/setting.rs` containing only the test module:

```rust
#[cfg(test)]
mod tests {
    use crate::db::Database;
    use crate::repositories::SettingsRepository;

    #[test]
    fn absent_key_reads_as_none() {
        let db = Database::open_in_memory().unwrap();
        let settings = SettingsRepository::new(&db);
        assert_eq!(settings.get("onboarding_completed_at").unwrap(), None);
    }

    #[test]
    fn a_value_survives_a_round_trip() {
        let db = Database::open_in_memory().unwrap();
        let settings = SettingsRepository::new(&db);

        settings.set("onboarding_completed_at", "2026-08-13T10:00:00Z").unwrap();

        assert_eq!(
            settings.get("onboarding_completed_at").unwrap().as_deref(),
            Some("2026-08-13T10:00:00Z")
        );
    }

    /// `set` is an upsert. Onboarding writes the same key on every completion attempt, and a
    /// UNIQUE violation there would turn a harmless retry into an error.
    #[test]
    fn setting_an_existing_key_overwrites_it() {
        let db = Database::open_in_memory().unwrap();
        let settings = SettingsRepository::new(&db);

        settings.set("k", "first").unwrap();
        settings.set("k", "second").unwrap();

        assert_eq!(settings.get("k").unwrap().as_deref(), Some("second"));
    }

    #[test]
    fn a_deleted_key_reads_as_none_again() {
        let db = Database::open_in_memory().unwrap();
        let settings = SettingsRepository::new(&db);

        settings.set("k", "v").unwrap();
        settings.delete("k").unwrap();

        assert_eq!(settings.get("k").unwrap(), None);
    }
}
```

Add to `core/crates/storage/src/repositories/mod.rs`, in the existing alphabetical `mod` block (after `mod search;`) and the `pub use` block (after the `search` line):

```rust
mod setting;
```

```rust
pub use setting::SettingsRepository;
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p notewise-storage setting`
Expected: FAIL — `cannot find type SettingsRepository in this scope`.

- [ ] **Step 3: Add the migration**

In `core/crates/storage/src/migrations.rs`, append a new element to the end of the `MIGRATIONS` array (after the final existing `r#"..."#` entry, before the closing `];`):

```rust
    // Application-level configuration. A generic key/value table rather than an `onboarding`
    // table: this is not a domain object, it has no edges, and the next setting to need
    // persisting should not require another migration.
    r#"
    CREATE TABLE app_settings (
        key        TEXT PRIMARY KEY,
        value      TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    "#,
```

`SUPPORTED_VERSION` is `MIGRATIONS.len()`, so it advances by itself.

- [ ] **Step 4: Write the repository**

Prepend to `core/crates/storage/src/repositories/setting.rs`, above the test module:

```rust
use chrono::Utc;

use crate::db::Database;
use crate::error::Result;

/// Application-level key/value configuration.
///
/// Deliberately untyped: values are strings, and the caller owns the meaning. A typed column
/// per setting would mean a migration every time the app grows a preference, for no benefit —
/// nothing joins against these.
#[derive(Debug)]
pub struct SettingsRepository<'a> {
    db: &'a Database,
}

impl<'a> SettingsRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// The value for `key`, or `None` when it has never been set.
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare("SELECT value FROM app_settings WHERE key = ?1")?;
        let mut rows = stmt.query(rusqlite::params![key])?;

        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Insert or overwrite `key`.
    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db.conn().execute(
            "INSERT INTO app_settings (key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                            updated_at = excluded.updated_at",
            rusqlite::params![key, value, Utc::now()],
        )?;
        Ok(())
    }

    /// Remove `key`. Removing an absent key succeeds.
    pub fn delete(&self, key: &str) -> Result<()> {
        self.db
            .conn()
            .execute("DELETE FROM app_settings WHERE key = ?1", rusqlite::params![key])?;
        Ok(())
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p notewise-storage setting`
Expected: PASS — 4 tests.

Run: `cargo test -p notewise-storage migrations`
Expected: PASS — the existing migration tests still hold at the new `SUPPORTED_VERSION`.

- [ ] **Step 6: Commit**

```bash
git add core/crates/storage/src/migrations.rs core/crates/storage/src/repositories/setting.rs core/crates/storage/src/repositories/mod.rs
git commit -m "feat(storage): app_settings key/value table and repository"
```

---

## Task 2: `AiBackend::probe()`

**Files:**
- Modify: `core/crates/ai-router/src/lib.rs`
- Modify: `core/crates/ai-router/src/backends/ollama.rs`
- Modify: `core/crates/ai-router/src/router.rs`

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` in `core/crates/ai-router/src/lib.rs`:

```rust
    /// Cloud backends inherit the default: a configured API key is the check. Issuing a real
    /// completion on every launch to prove reachability would cost money and seconds.
    #[tokio::test]
    async fn the_default_probe_succeeds_without_a_network_call() {
        let backend = crate::backends::mock::MockBackend::default();
        assert!(backend.probe().await.is_ok());
    }
```

Append to the `#[cfg(test)] mod tests` in `core/crates/ai-router/src/backends/ollama.rs` (create the module at the end of the file if none exists):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A refused connection is a reachability answer, not a panic. Port 1 is reserved and
    /// nothing listens on it, so this exercises the error path without a network.
    #[tokio::test]
    async fn probe_reports_an_unreachable_daemon() {
        let backend = OllamaBackend::new().with_endpoint("http://127.0.0.1:1/api/chat");
        let err = backend.probe().await.expect_err("nothing listens on port 1");
        assert!(matches!(err, AiError::Transport { .. }), "got {err:?}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p notewise-ai-router probe`
Expected: FAIL — `no method named 'probe' found`.

- [ ] **Step 3: Add the trait method**

In `core/crates/ai-router/src/lib.rs`, inside `pub trait AiBackend`, after the `chat` method:

```rust
    /// Whether this backend is usable right now.
    ///
    /// The default answers yes without a network call, which is correct for every hosted
    /// provider: the backend was constructed, so a key was present, and spending a real
    /// completion to prove the endpoint is up would cost money on every launch. Local
    /// backends that depend on a separate daemon override this.
    async fn probe(&self) -> Result<()> {
        Ok(())
    }
```

- [ ] **Step 4: Override it for Ollama**

In `core/crates/ai-router/src/backends/ollama.rs`, inside `impl AiBackend for OllamaBackend`, after the `chat` method:

```rust
    /// Ask the daemon for its model list.
    ///
    /// `/api/tags` is a cheap GET that touches no model. The configured endpoint points at
    /// `/api/chat`, so the base is recovered by trimming that suffix rather than by storing
    /// a second URL that could drift out of sync with the first.
    async fn probe(&self) -> Result<()> {
        let base = self
            .endpoint
            .strip_suffix("/api/chat")
            .unwrap_or(&self.endpoint);

        let response = self
            .http
            .get(format!("{base}/api/tags"))
            // Bounded, because this runs on the setup screen's critical path: an installed
            // but stopped daemon must answer "not reachable" in a moment, not hang until the
            // OS connect timeout expires.
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .map_err(|source| AiError::Transport {
                backend: BACKEND,
                source,
            })?;

        if !response.status().is_success() {
            return Err(AiError::Provider {
                backend: BACKEND,
                status: response.status().as_u16(),
                message: "the Ollama daemon did not return its model list".into(),
            });
        }

        Ok(())
    }
```

- [ ] **Step 5: Delegate from the router**

In `core/crates/ai-router/src/router.rs`, inside `impl Router`, after the existing delegating methods:

```rust
    /// Whether the active backend is usable right now. See [`AiBackend::probe`].
    pub async fn probe(&self) -> Result<()> {
        self.backend.probe().await
    }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p notewise-ai-router probe`
Expected: PASS — 2 tests.

- [ ] **Step 7: Commit**

```bash
git add core/crates/ai-router/src
git commit -m "feat(ai-router): probe() for backend reachability"
```

---

## Task 3: Permission probes in `audio-capture`

**Files:**
- Create: `core/crates/audio-capture/src/permissions.rs`
- Modify: `core/crates/audio-capture/src/lib.rs`
- Modify: `core/crates/audio-capture/src/microphone.rs:290-301`

- [ ] **Step 1: Write the failing test**

Create `core/crates/audio-capture/src/permissions.rs` containing only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// System audio has no working backend on any current build, so it must report why
    /// rather than claim to be merely un-asked. A caller that cannot distinguish these two
    /// would block a user on a grant that does not exist.
    #[test]
    fn system_audio_is_unavailable_with_a_stated_reason() {
        match permission_status(CaptureKind::SystemAudio) {
            PermissionStatus::Unavailable(reason) => assert!(
                !reason.is_empty(),
                "an unavailable capability must say why"
            ),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    /// Nothing has prompted yet, so the honest answer is `NotRequested` — not a guess derived
    /// from whether devices happen to enumerate.
    #[test]
    #[cfg(feature = "os-capture")]
    fn microphone_starts_out_unrequested() {
        assert_eq!(
            permission_status(CaptureKind::Microphone),
            PermissionStatus::NotRequested
        );
    }

    #[test]
    #[cfg(not(feature = "os-capture"))]
    fn microphone_is_unavailable_without_the_capture_feature() {
        assert!(matches!(
            permission_status(CaptureKind::Microphone),
            PermissionStatus::Unavailable(_)
        ));
    }

    #[test]
    fn permission_error_strings_are_recognised() {
        assert!(is_permission_error("Permission denied"));
        assert!(is_permission_error("the user DENIED access"));
        assert!(!is_permission_error("device disconnected"));
    }

    /// Requires a real device and, on macOS, a TCC grant against a signed bundle. Neither
    /// exists in CI, and a green run must not imply this was verified.
    #[test]
    #[ignore = "requires a microphone and an OS permission grant"]
    #[cfg(feature = "os-capture")]
    fn requesting_the_microphone_reaches_a_terminal_answer() {
        assert!(matches!(
            request_permission(CaptureKind::Microphone),
            PermissionStatus::Granted | PermissionStatus::Denied
        ));
    }
}
```

Add to `core/crates/audio-capture/src/lib.rs`, beside the existing module declarations:

```rust
pub mod permissions;
```

and beside the existing re-exports:

```rust
pub use permissions::{PermissionStatus, request_permission, permission_status};
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p notewise-audio-capture permissions`
Expected: FAIL — `file not found for module permissions` is resolved but `cannot find function status`.

- [ ] **Step 3: Write the module**

Prepend to `core/crates/audio-capture/src/permissions.rs`, above the test module:

```rust
//! Whether this machine will actually let us capture.
//!
//! Every function here touches the thing it reports on. A permission check that infers a
//! grant from device enumeration is worse than no check: devices enumerate fine while the
//! grant is denied, so the user is shown a green tick and then records silence.

use crate::{CaptureKind, OsBackend};

/// What the OS will let us do, as far as we have actually asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionStatus {
    /// Obtainable, but nothing has prompted yet. Not a guess — a statement that we have not
    /// asked, so the UI can offer the button rather than fake an answer.
    NotRequested,
    Granted,
    Denied,
    /// Cannot be granted on this build or platform, with the reason. Callers must not gate
    /// on this: there is no action a user could take.
    Unavailable(String),
}

/// Report a capability without prompting.
///
/// Safe to call on page load. It never opens a device, because doing so would raise an OS
/// dialog before the user pressed anything.
pub fn permission_status(kind: CaptureKind) -> PermissionStatus {
    if let Some(reason) = unavailable_reason(kind) {
        return PermissionStatus::Unavailable(reason);
    }
    PermissionStatus::NotRequested
}

/// Ask for a capability, prompting if the OS decides to.
///
/// Blocking: it opens an audio device. Callers on an async runtime must use `spawn_blocking`.
pub fn request_permission(kind: CaptureKind) -> PermissionStatus {
    if let Some(reason) = unavailable_reason(kind) {
        return PermissionStatus::Unavailable(reason);
    }
    probe(kind)
}

/// Why `kind` cannot be granted here, or `None` when it can.
fn unavailable_reason(kind: CaptureKind) -> Option<String> {
    if !cfg!(feature = "os-capture") {
        return Some("this build has no capture support (built without the 'record' feature)".into());
    }

    match kind {
        // The microphone path is `cpal`, not `OsBackend` — it needs no signed bundle and is
        // the one capability obtainable everywhere `cpal` runs.
        CaptureKind::Microphone => None,
        _ => match OsBackend::for_host(kind) {
            None => Some(format!(
                "{} capture is not supported on {}",
                kind.as_str(),
                std::env::consts::OS
            )),
            Some(backend) => backend.unavailable_reason().map(str::to_string),
        },
    }
}

/// Open the device briefly and map the outcome.
#[cfg(feature = "os-capture")]
fn probe(kind: CaptureKind) -> PermissionStatus {
    use crate::{CaptureConfig, MicrophoneSource};

    match kind {
        CaptureKind::Microphone => {
            // Opening and immediately dropping is the whole probe: on macOS this is what
            // raises the TCC dialog on first call, and what returns a permission error once
            // the user has declined.
            match MicrophoneSource::open(&CaptureConfig::default()) {
                Ok(source) => {
                    drop(source);
                    PermissionStatus::Granted
                }
                Err(crate::CaptureError::PermissionDenied { .. }) => PermissionStatus::Denied,
                Err(e) if is_permission_error(&e.to_string()) => PermissionStatus::Denied,
                // A missing device is not a denied permission, and telling a user to visit
                // System Settings over an unplugged microphone would send them nowhere useful.
                Err(e) => PermissionStatus::Unavailable(e.to_string()),
            }
        }
        _ => PermissionStatus::Unavailable("no probe for this capability".into()),
    }
}

#[cfg(not(feature = "os-capture"))]
fn probe(_kind: CaptureKind) -> PermissionStatus {
    PermissionStatus::Unavailable("this build has no capture support".into())
}

/// Whether an OS error string describes a refused permission.
///
/// String matching because `cpal` surfaces the platform's own message rather than a typed
/// error. Shared with `microphone.rs` so the two cannot disagree about what a denial is.
pub(crate) fn is_permission_error(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("permission") || message.contains("denied")
}
```

- [ ] **Step 4: Share the mapping with `microphone.rs`**

In `core/crates/audio-capture/src/microphone.rs`, replace the closure body at lines 290-301:

```rust
    .map_err(|e| {
        // A denied microphone permission surfaces here on macOS.
        let message = e.to_string();
        if message.to_lowercase().contains("permission")
            || message.to_lowercase().contains("denied")
        {
            CaptureError::PermissionDenied { what: "microphone" }
        } else {
            CaptureError::BadFormat(format!("building input stream: {message}"))
        }
    })?;
```

with:

```rust
    .map_err(|e| {
        // A denied microphone permission surfaces here on macOS. The test for what counts as
        // a denial lives in `permissions` so the probe and the capture path cannot drift.
        let message = e.to_string();
        if crate::permissions::is_permission_error(&message) {
            CaptureError::PermissionDenied { what: "microphone" }
        } else {
            CaptureError::BadFormat(format!("building input stream: {message}"))
        }
    })?;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p notewise-audio-capture permissions`
Expected: PASS — 3 tests run, 1 ignored.

Run: `cargo test -p notewise-audio-capture --features os-capture permissions`
Expected: PASS — 3 tests run, 1 ignored.

- [ ] **Step 6: Commit**

```bash
git add core/crates/audio-capture/src
git commit -m "feat(audio-capture): permission probes that open the device"
```

---

## Task 4: Fix the model directory `list_models` reads

**Files:**
- Modify: `core/crates/api-server/src/routes.rs:1011-1034`

This is a prerequisite, not a drive-by. `AppState::with_model_dir` points the desktop app at
`<app_data>/models` (`apps/desktop/src-tauri/src/main.rs:138`), while the free `model_store()`
guesses a platform path from environment variables. `/v1/models` uses the guess and
`download_model` uses the configured directory, so the Model step could show a downloaded
model as missing and never satisfy its gate.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` at the end of `core/crates/api-server/src/routes.rs`.
Match the surrounding tests' helper style — they build state with `.with_model_dir(dir.path())`
around line 2424:

```rust
    /// `/v1/models` and `/v1/models/:name/download` must agree about where models live.
    /// They did not: one honoured `with_model_dir`, the other re-derived a path from the
    /// environment, so a model on disk could be reported as missing forever.
    /// Install a model fixture the store will actually accept.
    ///
    /// `ModelStore::is_available` compares the file's byte length against the catalogue
    /// (`models.rs:176`), so a short placeholder does not register as installed. `set_len`
    /// produces a sparse file — correct length, no bytes written — which matters because
    /// even the smallest model in the registry is tens of megabytes.
    fn install_smallest_model(dir: &std::path::Path) -> notewise_transcription::ModelInfo {
        let model = notewise_transcription::ModelRegistry::all()
            .into_iter()
            .min_by_key(|m| m.bytes)
            .expect("the registry is not empty");

        let store = notewise_transcription::ModelStore::new(dir);
        std::fs::File::create(store.path_for(&model))
            .unwrap()
            .set_len(model.bytes)
            .unwrap();

        model
    }

    #[tokio::test]
    async fn list_models_honours_the_configured_model_dir() {
        let dir = tempfile::tempdir().unwrap();
        let model = install_smallest_model(dir.path());

        let app = crate::routes::router(std::sync::Arc::new(
            AppState::new(
                notewise_storage::Database::open_in_memory().unwrap(),
                notewise_ai_router::Router::from_config(
                    notewise_ai_router::RouterConfig::mock(),
                )
                .unwrap(),
            )
            .with_model_dir(dir.path()),
        ));

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/models")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            json["directory"].as_str().unwrap(),
            dir.path().display().to_string(),
            "the listing must report the configured directory"
        );

        let listed = json["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == model.name.as_str())
            .expect("the default model is in the registry");

        assert_eq!(
            listed["installed"], true,
            "a model written into the configured directory must list as installed"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p notewise-api-server list_models_honours`
Expected: FAIL — `installed` is `false` and `directory` is the environment-derived guess.

- [ ] **Step 3: Fix the handler**

In `core/crates/api-server/src/routes.rs`, replace lines 1011-1013:

```rust
async fn list_models(State(state): State<Shared>) -> ApiResult<Json<serde_json::Value>> {
    let store = model_store();
    let _ = &state; // model storage is independent of the database
```

with:

```rust
async fn list_models(State(state): State<Shared>) -> ApiResult<Json<serde_json::Value>> {
    // `state.model_store()`, not the environment-derived `model_store()`: the desktop shell
    // configures a directory the free function does not know about, and a listing that
    // disagrees with the downloader reports installed models as missing.
    let store = state.model_store();
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p notewise-api-server list_models`
Expected: PASS.

- [ ] **Step 5: Check whether `model_store()` is now dead**

Run: `cargo clippy -p notewise-api-server --all-targets -- -D warnings`

If clippy reports `model_store` as never used, delete the function at `routes.rs:1138-1160`.
If other call sites remain, leave it.

- [ ] **Step 6: Commit**

```bash
git add core/crates/api-server/src/routes.rs
git commit -m "fix(api-server): list_models read the wrong model directory"
```

---

## Task 5: The readiness snapshot

**Files:**
- Create: `core/crates/api-server/src/setup.rs`
- Modify: `core/crates/api-server/src/lib.rs`

Policy lives here, away from HTTP, so it can be tested without a request.

- [ ] **Step 1: Write the failing test**

Create `core/crates/api-server/src/setup.rs` containing only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn permission(status: &str, required: bool) -> PermissionReadiness {
        PermissionReadiness {
            status: status.into(),
            required,
            detail: None,
        }
    }

    #[test]
    fn an_unavailable_capability_is_never_required() {
        let readiness = PermissionsReadiness::from_parts(
            permission("granted", true),
            PermissionReadiness {
                status: "unavailable".into(),
                required: false,
                detail: Some("no signed bundle".into()),
            },
        );

        assert!(readiness.satisfied, "an ungrantable capability must not block");
    }

    #[test]
    fn a_denied_required_capability_blocks() {
        let readiness =
            PermissionsReadiness::from_parts(permission("denied", true), permission("unavailable", false));

        assert!(!readiness.satisfied);
    }

    #[test]
    fn an_unrequested_required_capability_blocks() {
        let readiness = PermissionsReadiness::from_parts(
            permission("not_requested", true),
            permission("unavailable", false),
        );

        assert!(!readiness.satisfied);
    }

    #[test]
    fn unsatisfied_steps_are_named_so_a_409_can_say_which() {
        let setup = SetupReadiness {
            completed_at: None,
            steps: Steps {
                model: StepReadiness { satisfied: false, required: true },
                backend: StepReadiness { satisfied: true, required: true },
                permissions: PermissionsReadiness::from_parts(
                    permission("denied", true),
                    permission("unavailable", false),
                ),
            },
        };

        assert_eq!(setup.unsatisfied(), vec!["model", "permissions"]);
    }

    #[test]
    fn everything_satisfied_leaves_nothing_unsatisfied() {
        let setup = SetupReadiness {
            completed_at: None,
            steps: Steps {
                model: StepReadiness { satisfied: true, required: true },
                backend: StepReadiness { satisfied: true, required: true },
                permissions: PermissionsReadiness::from_parts(
                    permission("granted", true),
                    permission("unavailable", false),
                ),
            },
        };

        assert!(setup.unsatisfied().is_empty());
    }
}
```

Add to `core/crates/api-server/src/lib.rs`, beside the other module declarations:

```rust
mod setup;
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p notewise-api-server setup`
Expected: FAIL — `cannot find type SetupReadiness in this scope`.

- [ ] **Step 3: Write the module**

Prepend to `core/crates/api-server/src/setup.rs`, above the test module:

```rust
//! What first-run setup still needs, and which of it applies to this build.
//!
//! Kept apart from the route table because it is policy, not transport: whether a capability
//! blocks the user is a decision worth testing without constructing a request.

use serde::Serialize;

/// The key under which completion is recorded in `app_settings`.
pub(crate) const COMPLETED_KEY: &str = "onboarding_completed_at";

#[derive(Debug, Serialize)]
pub(crate) struct SetupReadiness {
    /// RFC 3339, or `None` while setup has never been finished.
    pub completed_at: Option<String>,
    pub steps: Steps,
}

#[derive(Debug, Serialize)]
pub(crate) struct Steps {
    pub model: StepReadiness,
    pub backend: StepReadiness,
    pub permissions: PermissionsReadiness,
}

#[derive(Debug, Serialize)]
pub(crate) struct StepReadiness {
    pub satisfied: bool,
    pub required: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PermissionReadiness {
    /// `not_requested` | `granted` | `denied` | `unavailable`.
    pub status: String,
    pub required: bool,
    /// Why it is unavailable, when it is. Shown verbatim.
    pub detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PermissionsReadiness {
    pub satisfied: bool,
    pub required: bool,
    pub microphone: PermissionReadiness,
    pub system_audio: PermissionReadiness,
}

impl PermissionsReadiness {
    /// Combine per-capability answers into one gate.
    ///
    /// Only `granted` satisfies a required capability. `unavailable` is excluded from the
    /// gate entirely — there is no action a user could take, so blocking on it would trap
    /// them. That is the whole of the "required only when available" rule.
    pub fn from_parts(microphone: PermissionReadiness, system_audio: PermissionReadiness) -> Self {
        let satisfied = [&microphone, &system_audio]
            .into_iter()
            .filter(|p| p.required)
            .all(|p| p.status == "granted");

        let required = microphone.required || system_audio.required;

        Self {
            satisfied,
            required,
            microphone,
            system_audio,
        }
    }
}

impl SetupReadiness {
    /// The names of required steps that are not satisfied, in wizard order.
    ///
    /// Returned rather than a bare bool so a rejected completion can say which step is
    /// missing instead of "setup incomplete".
    pub fn unsatisfied(&self) -> Vec<&'static str> {
        let mut names = Vec::new();

        if self.steps.model.required && !self.steps.model.satisfied {
            names.push("model");
        }
        if self.steps.backend.required && !self.steps.backend.satisfied {
            names.push("backend");
        }
        if self.steps.permissions.required && !self.steps.permissions.satisfied {
            names.push("permissions");
        }

        names
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p notewise-api-server setup`
Expected: PASS — 5 tests.

- [ ] **Step 5: Commit**

```bash
git add core/crates/api-server/src/setup.rs core/crates/api-server/src/lib.rs
git commit -m "feat(api-server): setup readiness policy"
```

---

## Task 6: `GET /v1/setup`

**Files:**
- Modify: `core/crates/api-server/src/routes.rs`

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` in `core/crates/api-server/src/routes.rs`:

```rust
    #[tokio::test]
    async fn setup_reports_an_unfinished_first_run() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::routes::router(std::sync::Arc::new(
            AppState::new(
                notewise_storage::Database::open_in_memory().unwrap(),
                notewise_ai_router::Router::from_config(
                    notewise_ai_router::RouterConfig::mock(),
                )
                .unwrap(),
            )
            .with_model_dir(dir.path()),
        ));

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/setup")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json["completed_at"].is_null(), "nothing has completed setup");
        assert_eq!(json["steps"]["model"]["satisfied"], false, "empty model dir");
        assert_eq!(json["steps"]["model"]["required"], true);

        // System audio has no working backend on any current build, so it must be excluded
        // from the gate rather than left permanently blocking.
        assert_eq!(json["steps"]["permissions"]["system_audio"]["status"], "unavailable");
        assert_eq!(json["steps"]["permissions"]["system_audio"]["required"], false);
        assert!(json["steps"]["permissions"]["system_audio"]["detail"].is_string());
    }

    /// A GET must never raise a TCC dialog. The only defence in code is that the handler
    /// calls the non-prompting probe, so pin the status it produces.
    #[tokio::test]
    async fn setup_does_not_prompt_for_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::routes::router(std::sync::Arc::new(
            AppState::new(
                notewise_storage::Database::open_in_memory().unwrap(),
                notewise_ai_router::Router::from_config(
                    notewise_ai_router::RouterConfig::mock(),
                )
                .unwrap(),
            )
            .with_model_dir(dir.path()),
        ));

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/setup")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let status = json["steps"]["permissions"]["microphone"]["status"]
            .as_str()
            .unwrap();
        assert!(
            status == "not_requested" || status == "unavailable",
            "a GET must not have asked the OS anything, got {status}"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p notewise-api-server setup_reports`
Expected: FAIL — 404, because the route does not exist.

- [ ] **Step 3: Register the route**

In `core/crates/api-server/src/routes.rs`, add to the router chain after the `/v1/downloads` line:

```rust
        .route("/v1/setup", get(setup_readiness))
```

- [ ] **Step 4: Write the handler and its helpers**

Add near the other handlers in `core/crates/api-server/src/routes.rs`:

```rust
/// What first-run setup still needs.
///
/// Never prompts. It reads permission status without opening a device, so loading the
/// wizard cannot raise an OS dialog before the user has pressed anything.
async fn setup_readiness(State(state): State<Shared>) -> ApiResult<Json<crate::setup::SetupReadiness>> {
    Ok(Json(readiness(&state).await?))
}

async fn readiness(state: &AppState) -> ApiResult<crate::setup::SetupReadiness> {
    use crate::setup::{PermissionsReadiness, SetupReadiness, StepReadiness, Steps, COMPLETED_KEY};

    let completed_at = {
        let db = state.db().await;
        notewise_storage::SettingsRepository::new(&db).get(COMPLETED_KEY)?
    };

    let store = state.model_store();
    let model_installed = !store.installed().is_empty();

    let ai = state.ai();
    let backend_reachable = ai.probe().await.is_ok();

    Ok(SetupReadiness {
        completed_at,
        steps: Steps {
            model: StepReadiness {
                satisfied: model_installed,
                required: true,
            },
            backend: StepReadiness {
                satisfied: backend_reachable,
                required: true,
            },
            permissions: PermissionsReadiness::from_parts(
                permission_readiness(CaptureKindArg::Microphone, false),
                permission_readiness(CaptureKindArg::SystemAudio, false),
            ),
        },
    })
}

/// Which capability a permission route is about.
///
/// A local enum rather than re-exporting `CaptureKind`, because `audio-capture` is an
/// optional dependency and the route table must compile without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureKindArg {
    Microphone,
    SystemAudio,
}

impl CaptureKindArg {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "microphone" => Some(Self::Microphone),
            "system_audio" => Some(Self::SystemAudio),
            _ => None,
        }
    }
}

/// Report one capability. `prompt` opens a device and may raise an OS dialog.
fn permission_readiness(kind: CaptureKindArg, prompt: bool) -> crate::setup::PermissionReadiness {
    #[cfg(feature = "record")]
    {
        use notewise_audio_capture::{CaptureKind, PermissionStatus};

        let kind = match kind {
            CaptureKindArg::Microphone => CaptureKind::Microphone,
            CaptureKindArg::SystemAudio => CaptureKind::SystemAudio,
        };

        let status = if prompt {
            notewise_audio_capture::request_permission(kind)
        } else {
            notewise_audio_capture::permission_status(kind)
        };

        let (status, detail) = match status {
            PermissionStatus::NotRequested => ("not_requested", None),
            PermissionStatus::Granted => ("granted", None),
            PermissionStatus::Denied => ("denied", None),
            PermissionStatus::Unavailable(reason) => ("unavailable", Some(reason)),
        };

        crate::setup::PermissionReadiness {
            status: status.into(),
            // Only an obtainable permission gates the user. Anything unavailable has no
            // action behind it, so requiring it would be a trap.
            required: status != "unavailable",
            detail,
        }
    }

    #[cfg(not(feature = "record"))]
    {
        let _ = (kind, prompt);
        crate::setup::PermissionReadiness {
            status: "unavailable".into(),
            required: false,
            detail: Some("this build has no capture support (built without the 'record' feature)".into()),
        }
    }
}
```

Add `SettingsRepository` to the existing `use notewise_storage::{...}` import list at the top
of `routes.rs`, keeping alphabetical order (after `SearchRepository,`).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p notewise-api-server setup`
Expected: PASS — 7 tests (5 from Task 5, 2 here).

- [ ] **Step 6: Commit**

```bash
git add core/crates/api-server/src/routes.rs
git commit -m "feat(api-server): GET /v1/setup readiness endpoint"
```

---

## Task 7: `POST /v1/setup/complete` and `POST /v1/permissions/:kind`

**Files:**
- Modify: `core/crates/api-server/src/routes.rs`

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` in `core/crates/api-server/src/routes.rs`:

```rust
    /// The gate must hold at the API, not only in the UI. A client calling this directly
    /// with nothing installed must be refused, and told which step is missing.
    #[tokio::test]
    async fn completing_setup_with_unsatisfied_steps_is_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::routes::router(std::sync::Arc::new(
            AppState::new(
                notewise_storage::Database::open_in_memory().unwrap(),
                notewise_ai_router::Router::from_config(
                    notewise_ai_router::RouterConfig::mock(),
                )
                .unwrap(),
            )
            .with_model_dir(dir.path()),
        ));

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/setup/complete")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);

        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(
            json["error"].as_str().unwrap().contains("model"),
            "the refusal must name the missing step, got {}",
            json["error"]
        );
    }

    #[tokio::test]
    async fn completing_setup_records_a_timestamp_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        // Same fixture helper as `list_models_honours_the_configured_model_dir`: the store
        // checks the file's exact length, so the model must be sparse-allocated to its
        // catalogue size to count as installed.
        install_smallest_model(dir.path());

        let state = std::sync::Arc::new(
            AppState::new(
                notewise_storage::Database::open_in_memory().unwrap(),
                notewise_ai_router::Router::from_config(
                    notewise_ai_router::RouterConfig::mock(),
                )
                .unwrap(),
            )
            .with_model_dir(dir.path()),
        );

        let complete = || {
            crate::routes::router(state.clone()).oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/setup/complete")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
        };

        let first = complete().await.unwrap();
        assert_eq!(first.status(), axum::http::StatusCode::OK);

        let body = http_body_util::BodyExt::collect(first.into_body())
            .await
            .unwrap()
            .to_bytes();
        let first_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let stamp = first_json["completed_at"].as_str().unwrap().to_string();
        assert!(!stamp.is_empty());

        let second = complete().await.unwrap();
        assert_eq!(second.status(), axum::http::StatusCode::OK);

        let body = http_body_util::BodyExt::collect(second.into_body())
            .await
            .unwrap()
            .to_bytes();
        let second_json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            second_json["completed_at"].as_str().unwrap(),
            stamp,
            "completing twice must not move the timestamp"
        );
    }

    #[tokio::test]
    async fn an_unknown_permission_kind_is_a_400() {
        let app = crate::routes::router(std::sync::Arc::new(AppState::new(
            notewise_storage::Database::open_in_memory().unwrap(),
            notewise_ai_router::Router::from_config(notewise_ai_router::RouterConfig::mock())
                .unwrap(),
        )));

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/webcam")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    /// On a build without capture, asking for system audio must answer "unavailable" rather
    /// than fail — the wizard needs a reason string to show, not an error banner.
    #[tokio::test]
    async fn requesting_system_audio_reports_unavailable_rather_than_failing() {
        let app = crate::routes::router(std::sync::Arc::new(AppState::new(
            notewise_storage::Database::open_in_memory().unwrap(),
            notewise_ai_router::Router::from_config(notewise_ai_router::RouterConfig::mock())
                .unwrap(),
        )));

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/system_audio")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], "unavailable");
        assert_eq!(json["required"], false);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p notewise-api-server completing_setup`
Expected: FAIL — 404 on both, the routes do not exist.

- [ ] **Step 3: Register the routes**

In `core/crates/api-server/src/routes.rs`, after the `/v1/setup` line added in Task 6:

```rust
        .route("/v1/setup/complete", post(complete_setup))
        .route("/v1/permissions/:kind", post(request_permission))
```

- [ ] **Step 4: Write the handlers**

Add beside `setup_readiness` in `core/crates/api-server/src/routes.rs`:

```rust
/// Mark first-run setup finished.
///
/// Re-checks readiness server-side. The wizard already disables its Finish button, but a
/// gate enforced only in the UI is not a gate.
async fn complete_setup(State(state): State<Shared>) -> ApiResult<Json<serde_json::Value>> {
    use crate::setup::COMPLETED_KEY;

    let readiness = readiness(&state).await?;

    // Already finished: answer with the original timestamp. Rewriting it would make a
    // double-click look like a second setup, and re-checking readiness could refuse a user
    // who legitimately completed setup before a model was later removed.
    if let Some(existing) = readiness.completed_at {
        return Ok(Json(serde_json::json!({ "completed_at": existing })));
    }

    let unsatisfied = readiness.unsatisfied();
    if !unsatisfied.is_empty() {
        return Err(ApiError::Conflict(format!(
            "setup is not finished: {} still {} attention",
            unsatisfied.join(", "),
            if unsatisfied.len() == 1 { "needs" } else { "need" }
        )));
    }

    let stamp = Utc::now().to_rfc3339();
    {
        let db = state.db().await;
        notewise_storage::SettingsRepository::new(&db).set(COMPLETED_KEY, &stamp)?;
    }

    tracing::info!("first-run setup completed");
    Ok(Json(serde_json::json!({ "completed_at": stamp })))
}

/// Ask the OS for a capability, prompting if it decides to.
///
/// Runs on a blocking thread: opening an audio device is not async, and holding a runtime
/// worker while a modal permission dialog waits on the user would stall every other request.
async fn request_permission(
    Path(kind): Path<String>,
) -> ApiResult<Json<crate::setup::PermissionReadiness>> {
    let kind = CaptureKindArg::parse(&kind).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "unknown permission '{kind}' — expected 'microphone' or 'system_audio'"
        ))
    })?;

    let readiness = tokio::task::spawn_blocking(move || permission_readiness(kind, true))
        .await
        .map_err(|e| ApiError::Internal(format!("the permission probe panicked: {e}")))?;

    Ok(Json(readiness))
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p notewise-api-server`
Expected: PASS — all existing tests plus the 4 new ones.

- [ ] **Step 6: Commit**

```bash
git add core/crates/api-server/src/routes.rs
git commit -m "feat(api-server): complete setup and request permissions"
```

---

## Task 8: Verify the engine end to end

- [ ] **Step 1: Format, lint, test the whole workspace**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: clean, all tests pass.

- [ ] **Step 2: Check the capture-enabled build compiles**

```bash
cargo clippy -p notewise-api-server --features record --all-targets -- -D warnings
```

Expected: clean. This is the build where `permission_readiness` takes its `#[cfg(feature = "record")]` arm — a typo there would otherwise only surface in a desktop bundle.

- [ ] **Step 3: Commit any formatting**

```bash
git add -A
git commit -m "style: cargo fmt" || echo "nothing to format"
```

---

## Task 9: Vitest and the pure readiness logic

**Files:**
- Modify: `apps/desktop/package.json`
- Create: `apps/desktop/src/onboarding/readiness.ts`
- Create: `apps/desktop/src/onboarding/readiness.test.ts`

- [ ] **Step 1: Add Vitest**

```bash
cd apps/desktop && npm install --save-dev vitest@^2.1.8
```

In `apps/desktop/package.json`, add to `scripts`:

```json
    "test": "vitest run",
```

- [ ] **Step 2: Write the failing test**

Create `apps/desktop/src/onboarding/readiness.test.ts`:

```ts
import { describe, expect, it } from "vitest";

import { canFinish, firstUnsatisfied, stepsFor, type SetupReadiness } from "./readiness";

/** A snapshot with everything satisfied, narrowed per test. */
function snapshot(overrides: Partial<SetupReadiness["steps"]> = {}): SetupReadiness {
  return {
    completed_at: null,
    steps: {
      model: { satisfied: true, required: true },
      backend: { satisfied: true, required: true },
      permissions: {
        satisfied: true,
        required: true,
        microphone: { status: "granted", required: true, detail: null },
        system_audio: { status: "unavailable", required: false, detail: "no signed bundle" },
      },
      ...overrides,
    },
  };
}

describe("stepsFor", () => {
  it("always offers all four steps, welcome first", () => {
    expect(stepsFor(snapshot()).map((s) => s.id)).toEqual([
      "welcome",
      "model",
      "backend",
      "permissions",
    ]);
  });

  it("marks welcome satisfied so it never blocks finishing", () => {
    const welcome = stepsFor(snapshot()).find((s) => s.id === "welcome");
    expect(welcome?.satisfied).toBe(true);
  });
});

describe("firstUnsatisfied", () => {
  it("lands on model when nothing is downloaded", () => {
    const next = firstUnsatisfied(
      snapshot({ model: { satisfied: false, required: true } }),
    );
    expect(next).toBe("model");
  });

  it("skips satisfied steps and lands on permissions", () => {
    const next = firstUnsatisfied(
      snapshot({
        permissions: {
          satisfied: false,
          required: true,
          microphone: { status: "not_requested", required: true, detail: null },
          system_audio: { status: "unavailable", required: false, detail: "x" },
        },
      }),
    );
    expect(next).toBe("permissions");
  });

  // Next from Welcome jumps to the first unsatisfied step. With nothing left to do it must
  // land on the last step rather than fall off the end, so Finish is reachable.
  it("lands on the last step when everything is already satisfied", () => {
    expect(firstUnsatisfied(snapshot())).toBe("permissions");
  });
});

describe("canFinish", () => {
  it("is true when every required step is satisfied", () => {
    expect(canFinish(snapshot())).toBe(true);
  });

  it("is false when a required step is unsatisfied", () => {
    expect(canFinish(snapshot({ model: { satisfied: false, required: true } }))).toBe(false);
  });

  // The rule the whole "required only when available" decision rests on: an ungrantable
  // capability must not be able to block the button forever.
  it("ignores an unavailable capability", () => {
    const withUnavailableSystemAudio = snapshot({
      permissions: {
        satisfied: true,
        required: true,
        microphone: { status: "granted", required: true, detail: null },
        system_audio: {
          status: "unavailable",
          required: false,
          detail: "ScreenCaptureKit requires a signed bundle",
        },
      },
    });
    expect(canFinish(withUnavailableSystemAudio)).toBe(true);
  });

  it("ignores a step that is not required, even when unsatisfied", () => {
    // A step that is not required cannot block. Pinned so a future edit does not turn
    // `required: false` into a hidden gate.
    expect(canFinish(snapshot({ backend: { satisfied: false, required: false } }))).toBe(true);
  });
});

describe("regression after completion", () => {
  it("reports the steps that regressed", () => {
    const completed: SetupReadiness = {
      ...snapshot({ model: { satisfied: false, required: true } }),
      completed_at: "2026-08-13T10:00:00Z",
    };
    expect(canFinish(completed)).toBe(false);
    expect(firstUnsatisfied(completed)).toBe("model");
  });
});
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd apps/desktop && npm test`
Expected: FAIL — `Failed to resolve import "./readiness"`.

- [ ] **Step 4: Write the module**

Create `apps/desktop/src/onboarding/readiness.ts`:

```ts
/**
 * Setup readiness, and the decisions derived from it.
 *
 * Deliberately pure — no React, no fetch. Which steps exist, which one to show, and whether
 * Finish enables are the only real branching in this feature, and they are worth testing
 * without mounting anything.
 */

export type PermissionState = "not_requested" | "granted" | "denied" | "unavailable";

export interface PermissionReadiness {
  status: PermissionState;
  required: boolean;
  detail: string | null;
}

export interface StepReadiness {
  satisfied: boolean;
  required: boolean;
}

export interface PermissionsReadiness extends StepReadiness {
  microphone: PermissionReadiness;
  system_audio: PermissionReadiness;
}

export interface SetupReadiness {
  /** RFC 3339, or null while setup has never been finished. */
  completed_at: string | null;
  steps: {
    model: StepReadiness;
    backend: StepReadiness;
    permissions: PermissionsReadiness;
  };
}

export type StepId = "welcome" | "model" | "backend" | "permissions";

export interface Step {
  id: StepId;
  title: string;
  satisfied: boolean;
  required: boolean;
}

/** The steps in wizard order. */
export function stepsFor(readiness: SetupReadiness): Step[] {
  const { model, backend, permissions } = readiness.steps;

  return [
    // Welcome has nothing to satisfy. It is marked satisfied rather than excluded so it can
    // still be rendered and navigated to without ever holding up the Finish button.
    { id: "welcome", title: "Welcome", satisfied: true, required: false },
    { id: "model", title: "Model", satisfied: model.satisfied, required: model.required },
    { id: "backend", title: "Backend", satisfied: backend.satisfied, required: backend.required },
    {
      id: "permissions",
      title: "Permissions",
      satisfied: permissions.satisfied,
      required: permissions.required,
    },
  ];
}

/**
 * Where Next from Welcome should land.
 *
 * Falls back to the last step rather than to Welcome, so a user who has already satisfied
 * everything reaches the screen with the Finish button on it.
 */
export function firstUnsatisfied(readiness: SetupReadiness): StepId {
  const steps = stepsFor(readiness).filter((step) => step.id !== "welcome");
  const next = steps.find((step) => step.required && !step.satisfied);
  return next?.id ?? steps[steps.length - 1].id;
}

/** Whether every required step is satisfied. */
export function canFinish(readiness: SetupReadiness): boolean {
  return stepsFor(readiness).every((step) => !step.required || step.satisfied);
}

/** Required steps that are not satisfied, for the post-completion banner. */
export function regressions(readiness: SetupReadiness): Step[] {
  return stepsFor(readiness).filter((step) => step.required && !step.satisfied);
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd apps/desktop && npm test`
Expected: PASS — 11 tests.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/package.json apps/desktop/package-lock.json apps/desktop/src/onboarding/
git commit -m "feat(desktop): setup readiness derivation with tests"
```

---

## Task 10: Extract the duplicated download machinery

**Files:**
- Create: `apps/desktop/src/lib/format.ts`
- Create: `apps/desktop/src/lib/useModelDownload.ts`
- Modify: `apps/desktop/src/views/SettingsView.tsx:1-11`, `:39-103`, `:231-261`

Extracted before `ModelStep` needs them, so the wizard consumes one definition rather than a
copy that drifts.

- [ ] **Step 1: Create the formatter**

Create `apps/desktop/src/lib/format.ts`:

```ts
/** Bytes as GB/MB. Model sizes span 77 MB to 3 GB, so one unit does not serve both. */
export function size(bytes: number): string {
  return bytes >= 1_000_000_000
    ? `${(bytes / 1_000_000_000).toFixed(1)} GB`
    : `${Math.round(bytes / 1_000_000)} MB`;
}
```

- [ ] **Step 2: Create the download hook**

Create `apps/desktop/src/lib/useModelDownload.ts`:

```ts
import { useEffect, useState } from "react";

import { api, ApiError, type DownloadState } from "./api";

export interface ModelDownload {
  /** The model currently downloading, or null. */
  downloading: string | null;
  progress: DownloadState | null;
  error: string | null;
  start: (name: string) => Promise<void>;
  clearError: () => void;
}

/**
 * Start and follow a model download.
 *
 * The engine owns the download, so this hook is only a view onto it: it re-attaches to one
 * already running when it mounts, which is what lets a user leave the screen and come back
 * without losing the progress bar.
 */
export function useModelDownload(onFinished: () => void | Promise<void>): ModelDownload {
  const [downloading, setDownloading] = useState<string | null>(null);
  const [progress, setProgress] = useState<DownloadState | null>(null);
  const [error, setError] = useState<string | null>(null);

  const finish = async () => {
    setDownloading(null);
    setProgress(null);
    await onFinished();
  };

  const fail = (message: string) => {
    setError(message);
    setDownloading(null);
    setProgress(null);
  };

  const start = async (name: string) => {
    setDownloading(name);
    setProgress(null);
    setError(null);

    try {
      const started = await api.downloadModel(name);

      // Already on disk: the POST answers `done` and there is nothing to stream.
      if (started.status === "done") {
        await finish();
        return;
      }

      setProgress(started);
      api.watchDownload(name, setProgress, () => void finish(), fail);
    } catch (e) {
      fail(e instanceof ApiError ? e.message : "Download failed.");
    }
  };

  // Recover a download already running when this mounted — the engine owns it, so navigating
  // away and back must not lose the progress bar.
  useEffect(() => {
    let cancel: (() => void) | undefined;

    void api.downloads().then((states) => {
      const running = states.find((s) => s.status === "downloading");
      if (!running) return;

      setDownloading(running.model);
      setProgress(running);
      cancel = api.watchDownload(running.model, setProgress, () => void finish(), fail);
    });

    return () => cancel?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return { downloading, progress, error, start, clearError: () => setError(null) };
}
```

- [ ] **Step 3: Rewrite SettingsView to use them**

In `apps/desktop/src/views/SettingsView.tsx`:

Replace lines 1-11 (imports and the local `size`) with:

```tsx
import { useEffect, useState } from "react";
import { Check, Cloud, Download, HardDrive, Loader2, ShieldAlert, ShieldCheck } from "lucide-react";

import { api, ApiError, type BackendInfo, type ModelInfo } from "../lib/api";
import { size } from "../lib/format";
import { useModelDownload } from "../lib/useModelDownload";
```

Replace lines 13-21 (the state declarations) with:

```tsx
export function SettingsView() {
  const [backends, setBackends] = useState<BackendInfo[]>([]);
  const [active, setActive] = useState<{ model: string; is_local: boolean } | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [directory, setDirectory] = useState("");
  const [error, setError] = useState<string | null>(null);
```

Delete the `download` function (lines 39-73) and the recovery `useEffect` (lines 77-103), and
insert after the `load`/`useEffect` pair:

```tsx
  const { downloading, progress, error: downloadError, start } = useModelDownload(load);
```

Change the error banner's condition at line 110 from `{error && (` to:

```tsx
        {(error ?? downloadError) && (
```

and its body from `{error}` to `{error ?? downloadError}`.

Change the download button's handler at line 208 from `onClick={() => download(model.name)}` to:

```tsx
                    onClick={() => void start(model.name)}
```

- [ ] **Step 4: Verify it compiles**

Run: `cd apps/desktop && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/lib/format.ts apps/desktop/src/lib/useModelDownload.ts apps/desktop/src/views/SettingsView.tsx
git commit -m "refactor(desktop): extract byte formatting and download orchestration"
```

---

## Task 11: API client for setup

**Files:**
- Modify: `apps/desktop/src/lib/api.ts`

- [ ] **Step 1: Add the calls**

In `apps/desktop/src/lib/api.ts`, add to the `api` object after `downloads`:

```ts
  /** What first-run setup still needs. Never prompts — see `requestPermission`. */
  setup: () => request<SetupReadiness>("/v1/setup"),

  /**
   * Mark setup finished.
   *
   * Rejects with a 409 when a required step is unsatisfied, so a UI bug cannot let someone
   * past the gate. Completing twice returns the original timestamp.
   */
  completeSetup: () =>
    request<{ completed_at: string }>("/v1/setup/complete", { method: "POST" }),

  /** Ask the OS for a capability. May raise a permission dialog. */
  requestPermission: (kind: "microphone" | "system_audio") =>
    request<PermissionReadiness>(`/v1/permissions/${kind}`, { method: "POST" }),
```

Add the type import as the **first statement in the file**, above the module doc comment's
closing block and before any interface declaration — ES module imports must be at the top:

```ts
import type { PermissionReadiness, SetupReadiness } from "../onboarding/readiness";
```

and re-export them beside the existing interface declarations, so callers can keep importing
every API type from one place:

```ts
export type { PermissionReadiness, SetupReadiness };
```

`readiness.ts` imports nothing from `api.ts`, so this does not create a cycle.

- [ ] **Step 2: Verify it compiles**

Run: `cd apps/desktop && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add apps/desktop/src/lib/api.ts
git commit -m "feat(desktop): setup endpoints in the API client"
```

---

## Task 12: Stepper and Welcome

**Files:**
- Create: `apps/desktop/src/onboarding/Stepper.tsx`
- Create: `apps/desktop/src/onboarding/steps/WelcomeStep.tsx`

- [ ] **Step 1: Write the stepper**

Create `apps/desktop/src/onboarding/Stepper.tsx`:

```tsx
import { Check } from "lucide-react";

import type { Step, StepId } from "./readiness";

interface StepperProps {
  steps: Step[];
  current: StepId;
}

/**
 * Dots and connectors across the top.
 *
 * Shows satisfaction, not visit history: a green tick means the requirement is met, so a
 * user who backtracks does not see a step un-complete itself.
 */
export function Stepper({ steps, current }: StepperProps) {
  return (
    <ol className="flex items-center justify-center gap-1.5" aria-label="Setup progress">
      {steps.map((step, index) => {
        const isCurrent = step.id === current;
        const isDone = step.satisfied && !isCurrent;

        return (
          <li key={step.id} className="flex items-center gap-1.5">
            <div
              aria-current={isCurrent ? "step" : undefined}
              title={step.title}
              className={`flex h-7 w-7 items-center justify-center rounded-full text-[11px] font-medium transition ${
                isDone
                  ? "bg-emerald-500 text-white"
                  : isCurrent
                    ? "bg-neutral-900 text-white"
                    : "bg-neutral-200 text-neutral-500"
              }`}
            >
              {isDone ? <Check size={14} aria-hidden /> : index + 1}
              <span className="sr-only">{step.title}</span>
            </div>

            {index < steps.length - 1 && (
              <span
                aria-hidden
                className={`h-px w-8 ${step.satisfied ? "bg-emerald-500" : "bg-neutral-200"}`}
              />
            )}
          </li>
        );
      })}
    </ol>
  );
}
```

- [ ] **Step 2: Write the welcome screen**

Create `apps/desktop/src/onboarding/steps/WelcomeStep.tsx`:

```tsx
import { Cpu, Lock, Network } from "lucide-react";

interface WelcomeStepProps {
  onNext: () => void;
}

const CLAIMS = [
  { icon: Lock, text: "Recordings and transcripts stay on this machine" },
  { icon: Network, text: "Meetings become notes, tickets, and drafts — linked" },
  { icon: Cpu, text: "Works offline. No account, no cloud required" },
];

export function WelcomeStep({ onNext }: WelcomeStepProps) {
  return (
    <div className="flex flex-col items-center text-center">
      <h1 className="text-[32px] font-semibold tracking-tight text-neutral-900">
        Welcome to Notewise
      </h1>
      <p className="mt-2 text-[15px] text-neutral-500">
        Record, transcribe, and understand your meetings — on your own hardware.
      </p>

      <ul className="mt-8 w-full max-w-md divide-y divide-hairline overflow-hidden rounded-xl border border-hairline bg-white text-left">
        {CLAIMS.map(({ icon: Icon, text }) => (
          <li key={text} className="flex items-center gap-3 px-4 py-3.5">
            <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-neutral-100">
              <Icon size={15} className="text-neutral-600" aria-hidden />
            </span>
            <span className="text-[13px] text-neutral-800">{text}</span>
          </li>
        ))}
      </ul>

      <button
        type="button"
        onClick={onNext}
        className="mt-8 w-full max-w-md rounded-lg bg-neutral-900 px-6 py-3 text-[14px]
                   font-medium text-white transition hover:bg-neutral-800"
      >
        Get started
      </button>

      <p className="mt-3 text-[12px] text-neutral-400">
        Setup takes a few minutes, mostly downloading.
      </p>
    </div>
  );
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd apps/desktop && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add apps/desktop/src/onboarding/Stepper.tsx apps/desktop/src/onboarding/steps/WelcomeStep.tsx
git commit -m "feat(desktop): setup stepper and welcome screen"
```

---

## Task 13: Model step

**Files:**
- Create: `apps/desktop/src/onboarding/steps/ModelStep.tsx`

- [ ] **Step 1: Write the screen**

Create `apps/desktop/src/onboarding/steps/ModelStep.tsx`:

```tsx
import { useEffect, useState } from "react";
import { Check, Download, Loader2, Mic } from "lucide-react";

import { api, type ModelInfo } from "../../lib/api";
import { size } from "../../lib/format";
import { useModelDownload } from "../../lib/useModelDownload";

interface ModelStepProps {
  satisfied: boolean;
  /** Re-read readiness after the download lands, so the gate opens. */
  onChanged: () => Promise<void>;
}

export function ModelStep({ satisfied, onChanged }: ModelStepProps) {
  const [model, setModel] = useState<ModelInfo | null>(null);
  const [listError, setListError] = useState<string | null>(null);

  const { downloading, progress, error, start } = useModelDownload(onChanged);

  useEffect(() => {
    void api
      .models()
      .then(({ models }) => {
        setModel(models.find((m) => m.recommended) ?? models[0] ?? null);
      })
      .catch(() => setListError("Could not read the model catalogue."));
  }, []);

  return (
    <div className="flex flex-col items-center text-center">
      <h1 className="text-[26px] font-semibold tracking-tight text-neutral-900">
        Transcription model
      </h1>
      <p className="mt-2 max-w-md text-[14px] text-neutral-500">
        Speech recognition runs on this machine, so the model has to live here too. This is a
        one-time download.
      </p>

      {(listError ?? error) && (
        <div
          role="alert"
          className="mt-6 w-full max-w-md rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-left text-[13px] text-amber-900"
        >
          {listError ?? error}
        </div>
      )}

      <div className="mt-8 w-full max-w-md rounded-xl border border-hairline bg-white p-4 text-left">
        <div className="flex items-center gap-3">
          <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-neutral-100">
            <Mic size={16} className="text-neutral-600" aria-hidden />
          </span>

          <div className="min-w-0 flex-1">
            <div className="text-[14px] font-medium text-neutral-900">
              {model?.name ?? "Loading…"}
            </div>
            {model && (
              // RAM is shown because it decides whether the model runs at all, which the
              // download size does not tell you.
              <div className="text-[11px] text-neutral-400">
                {size(model.bytes)} download · ~{model.approx_ram_mb} MB RAM
              </div>
            )}
          </div>

          {satisfied ? (
            <span className="flex shrink-0 items-center gap-1 text-[12px] text-emerald-600">
              <Check size={14} aria-hidden />
              Installed
            </span>
          ) : (
            <button
              type="button"
              disabled={!model || downloading !== null}
              onClick={() => model && void start(model.name)}
              className="flex shrink-0 items-center gap-1.5 rounded-full border border-hairline
                         px-3 py-1.5 text-[12px] text-neutral-700 transition
                         hover:bg-neutral-50 disabled:opacity-50"
            >
              {downloading ? (
                <>
                  <Loader2 size={13} className="animate-spin" aria-hidden />
                  Downloading
                </>
              ) : (
                <>
                  <Download size={13} aria-hidden />
                  Download
                </>
              )}
            </button>
          )}
        </div>

        {downloading && (
          <div className="mt-4">
            <div
              role="progressbar"
              aria-valuenow={progress?.percent ?? 0}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-label={`Downloading ${downloading}`}
              className="h-1.5 w-full overflow-hidden rounded-full bg-neutral-100"
            >
              <div
                className="h-full rounded-full bg-record transition-[width] duration-300"
                style={{ width: `${progress?.percent ?? 0}%` }}
              />
            </div>

            <div className="mt-1.5 flex items-baseline justify-between text-[11px] text-neutral-500">
              <span className="font-mono tabular-nums">
                {progress
                  ? `${size(progress.downloaded_bytes)} / ${size(progress.total_bytes)}`
                  : "starting…"}
              </span>
              <span className="font-mono tabular-nums font-semibold">
                {progress?.percent ?? 0}%
              </span>
            </div>

            <p className="mt-2 text-[11px] text-neutral-400">
              The engine owns this download — it resumes where it left off if the connection
              drops, and continues if you close this window.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd apps/desktop && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add apps/desktop/src/onboarding/steps/ModelStep.tsx
git commit -m "feat(desktop): model download step"
```

---

## Task 14: Backend step

**Files:**
- Create: `apps/desktop/src/onboarding/steps/BackendStep.tsx`

- [ ] **Step 1: Write the screen**

Create `apps/desktop/src/onboarding/steps/BackendStep.tsx`:

```tsx
import { useEffect, useState } from "react";
import { Cloud, HardDrive, RefreshCw } from "lucide-react";

import { api, ApiError, type BackendInfo } from "../../lib/api";

interface BackendStepProps {
  satisfied: boolean;
  onChanged: () => Promise<void>;
}

/** Keys are read from the engine's own environment, never sent over HTTP. */
const KEY_VARIABLES = [
  "ANTHROPIC_API_KEY",
  "GEMINI_API_KEY",
  "GROQ_API_KEY",
  "OPENROUTER_API_KEY",
];

export function BackendStep({ satisfied, onChanged }: BackendStepProps) {
  const [active, setActive] = useState<{ model: string; is_local: boolean } | null>(null);
  const [backends, setBackends] = useState<BackendInfo[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    try {
      const { backends: list, active: current } = await api.backends();
      setBackends(list);
      setActive(current);
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not read the backend list.");
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const recheck = async () => {
    setBusy(true);
    await load();
    await onChanged();
    setBusy(false);
  };

  const local = backends.find((b) => b.is_local);

  return (
    <div className="flex flex-col items-center text-center">
      <h1 className="text-[26px] font-semibold tracking-tight text-neutral-900">
        Summaries and chat
      </h1>
      <p className="mt-2 max-w-md text-[14px] text-neutral-500">
        Transcription is local either way. This decides where the language model runs.
      </p>

      {error && (
        <div
          role="alert"
          className="mt-6 w-full max-w-md rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-left text-[13px] text-amber-900"
        >
          {error}
        </div>
      )}

      <div className="mt-8 w-full max-w-md space-y-3 text-left">
        <div
          className={`rounded-xl border p-4 ${
            satisfied ? "border-emerald-200 bg-emerald-50" : "border-hairline bg-white"
          }`}
        >
          <div className="flex items-start gap-3">
            <HardDrive size={16} className="mt-0.5 shrink-0 text-emerald-600" aria-hidden />
            <div className="min-w-0 flex-1">
              <div className="text-[14px] font-medium text-neutral-900">
                {local?.label ?? "On this machine"}
              </div>
              <div className="mt-0.5 flex items-center gap-1.5 text-[12px]">
                <span
                  aria-hidden
                  className={`h-1.5 w-1.5 rounded-full ${
                    satisfied ? "bg-emerald-500" : "bg-amber-500"
                  }`}
                />
                <span className={satisfied ? "text-emerald-800" : "text-amber-800"}>
                  {satisfied
                    ? `Reachable — ${active?.model ?? "ready"}`
                    : "Not reachable. Is Ollama running?"}
                </span>
              </div>
              <p className="mt-2 text-[12px] text-neutral-500">
                Nothing leaves this machine. Notewise does not install or update Ollama —
                start it, then re-check.
              </p>
            </div>

            <button
              type="button"
              onClick={() => void recheck()}
              disabled={busy}
              className="flex shrink-0 items-center gap-1.5 rounded-full border border-hairline
                         bg-white px-3 py-1.5 text-[12px] text-neutral-700 transition
                         hover:bg-neutral-50 disabled:opacity-50"
            >
              <RefreshCw size={13} className={busy ? "animate-spin" : ""} aria-hidden />
              Re-check
            </button>
          </div>
        </div>

        <div className="rounded-xl border border-hairline bg-white p-4">
          <div className="flex items-start gap-3">
            <Cloud size={16} className="mt-0.5 shrink-0 text-neutral-400" aria-hidden />
            <div className="min-w-0 flex-1">
              <div className="text-[14px] font-medium text-neutral-900">Bring your own key</div>
              <p className="mt-0.5 text-[12px] text-neutral-500">
                Transcripts are sent to the provider you choose. Set one of these in the
                engine's environment and restart, then re-check:
              </p>
              <ul className="mt-2 flex flex-wrap gap-1.5">
                {KEY_VARIABLES.map((name) => (
                  <li
                    key={name}
                    className="rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-[11px] text-neutral-600"
                  >
                    {name}
                  </li>
                ))}
              </ul>
              <p className="mt-2 text-[11px] text-neutral-400">
                Keys are read by the engine directly. Notewise never sends one over HTTP, not
                even on loopback.
              </p>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd apps/desktop && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add apps/desktop/src/onboarding/steps/BackendStep.tsx
git commit -m "feat(desktop): backend detection step"
```

---

## Task 15: Permissions step

**Files:**
- Create: `apps/desktop/src/onboarding/steps/PermissionRow.tsx`
- Create: `apps/desktop/src/onboarding/steps/PermissionsStep.tsx`

- [ ] **Step 1: Write the row**

Create `apps/desktop/src/onboarding/steps/PermissionRow.tsx`:

```tsx
import type { ComponentType } from "react";
import { Check, Loader2, MinusCircle } from "lucide-react";

import type { PermissionReadiness } from "../readiness";

interface PermissionRowProps {
  icon: ComponentType<{ size?: number; className?: string }>;
  title: string;
  description: string;
  readiness: PermissionReadiness;
  busy: boolean;
  onEnable: () => void;
  onOpenSettings: () => void;
}

export function PermissionRow({
  icon: Icon,
  title,
  description,
  readiness,
  busy,
  onEnable,
  onOpenSettings,
}: PermissionRowProps) {
  const { status, detail } = readiness;

  return (
    <div className="flex items-start gap-3 bg-white px-4 py-3.5">
      <span className="mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-neutral-100">
        <Icon size={16} className="text-neutral-600" />
      </span>

      <div className="min-w-0 flex-1">
        <div className="text-[14px] font-medium text-neutral-900">{title}</div>
        <p className="mt-0.5 text-[12px] text-neutral-500">
          {status === "unavailable" ? detail : description}
        </p>
        {status === "denied" && (
          <p className="mt-1 text-[12px] text-amber-800">
            Declined. Grant it in System Settings, then re-check.
          </p>
        )}
      </div>

      {status === "granted" && (
        <span className="flex shrink-0 items-center gap-1 text-[12px] text-emerald-600">
          <Check size={14} aria-hidden />
          Granted
        </span>
      )}

      {/* Not blocking, and says so. There is no action behind an unavailable capability, so
          offering a button would only invite a user to keep pressing it. */}
      {status === "unavailable" && (
        <span className="flex shrink-0 items-center gap-1 text-[12px] text-neutral-400">
          <MinusCircle size={14} aria-hidden />
          Not available
        </span>
      )}

      {(status === "not_requested" || status === "denied") && (
        <div className="flex shrink-0 gap-1.5">
          {status === "denied" && (
            <button
              type="button"
              onClick={onOpenSettings}
              className="rounded-full border border-hairline px-3 py-1.5 text-[12px]
                         text-neutral-700 transition hover:bg-neutral-50"
            >
              Open Settings
            </button>
          )}
          <button
            type="button"
            onClick={onEnable}
            disabled={busy}
            className="flex items-center gap-1.5 rounded-full border border-hairline px-3 py-1.5
                       text-[12px] text-neutral-700 transition hover:bg-neutral-50
                       disabled:opacity-50"
          >
            {busy && <Loader2 size={13} className="animate-spin" aria-hidden />}
            {status === "denied" ? "Re-check" : "Enable"}
          </button>
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Write the screen**

Create `apps/desktop/src/onboarding/steps/PermissionsStep.tsx`:

```tsx
import { useState } from "react";
import { Mic, Volume2 } from "lucide-react";

import { api } from "../../lib/api";
import type { PermissionsReadiness } from "../readiness";
import { PermissionRow } from "./PermissionRow";

interface PermissionsStepProps {
  readiness: PermissionsReadiness;
  onChanged: () => Promise<void>;
}

/** macOS deep link to Privacy & Security. Harmless elsewhere — the OS ignores it. */
const PRIVACY_SETTINGS = "x-apple.systempreferences:com.apple.preference.security";

export function PermissionsStep({ readiness, onChanged }: PermissionsStepProps) {
  const [busy, setBusy] = useState<"microphone" | "system_audio" | null>(null);

  const enable = async (kind: "microphone" | "system_audio") => {
    setBusy(kind);
    try {
      await api.requestPermission(kind);
      await onChanged();
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="flex flex-col items-center text-center">
      <h1 className="text-[26px] font-semibold tracking-tight text-neutral-900">Permissions</h1>
      <p className="mt-2 max-w-md text-[14px] text-neutral-500">
        Notewise needs the operating system's permission to hear a meeting. Nothing is
        recorded until you press record.
      </p>

      <div className="mt-8 w-full max-w-md divide-y divide-hairline overflow-hidden rounded-xl border border-hairline text-left">
        <PermissionRow
          icon={Mic}
          title="Microphone"
          description="Captures your side of the conversation."
          readiness={readiness.microphone}
          busy={busy === "microphone"}
          onEnable={() => void enable("microphone")}
          onOpenSettings={() => {
            window.location.href = PRIVACY_SETTINGS;
          }}
        />
        <PermissionRow
          icon={Volume2}
          title="System audio"
          description="Captures everyone else, straight from the meeting app."
          readiness={readiness.system_audio}
          busy={busy === "system_audio"}
          onEnable={() => void enable("system_audio")}
          onOpenSettings={() => {
            window.location.href = PRIVACY_SETTINGS;
          }}
        />
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd apps/desktop && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add apps/desktop/src/onboarding/steps/PermissionRow.tsx apps/desktop/src/onboarding/steps/PermissionsStep.tsx
git commit -m "feat(desktop): permissions step"
```

---

## Task 16: Flow, gate, and banner

**Files:**
- Create: `apps/desktop/src/onboarding/SetupFlow.tsx`
- Create: `apps/desktop/src/onboarding/SetupBanner.tsx`
- Create: `apps/desktop/src/onboarding/SetupGate.tsx`
- Modify: `apps/desktop/src/App.tsx:296-298`, `:405-408`

- [ ] **Step 1: Write the flow**

Create `apps/desktop/src/onboarding/SetupFlow.tsx`:

```tsx
import { useState } from "react";
import { ArrowLeft, Loader2 } from "lucide-react";

import { api, ApiError } from "../lib/api";
import { canFinish, firstUnsatisfied, stepsFor, type SetupReadiness, type StepId } from "./readiness";
import { Stepper } from "./Stepper";
import { BackendStep } from "./steps/BackendStep";
import { ModelStep } from "./steps/ModelStep";
import { PermissionsStep } from "./steps/PermissionsStep";
import { WelcomeStep } from "./steps/WelcomeStep";

interface SetupFlowProps {
  readiness: SetupReadiness;
  /** Re-fetch readiness from the engine. */
  refresh: () => Promise<void>;
  onFinished: () => void;
}

export function SetupFlow({ readiness, refresh, onFinished }: SetupFlowProps) {
  // Session-local, not persisted. Persisting a step index is what forced Meetily's context
  // to guard every write against its own auto-save; the cost here is one extra click after a
  // relaunch, which is the cheaper trade.
  const [step, setStep] = useState<StepId>("welcome");
  const [finishing, setFinishing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const steps = stepsFor(readiness);
  const order: StepId[] = steps.map((s) => s.id);
  const index = order.indexOf(step);

  const finish = async () => {
    setFinishing(true);
    setError(null);
    try {
      await api.completeSetup();
      onFinished();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not finish setup.");
      setFinishing(false);
    }
  };

  const isLast = index === order.length - 1;
  const ready = canFinish(readiness);

  return (
    <div className="flex h-full flex-col overflow-y-auto bg-neutral-50">
      <div className="mx-auto flex w-full max-w-2xl flex-1 flex-col px-6 py-12">
        {step !== "welcome" && (
          <div className="mb-10">
            <Stepper steps={steps} current={step} />
          </div>
        )}

        <div className="flex flex-1 flex-col justify-center">
          {step === "welcome" && (
            // Next jumps to the first thing actually outstanding, so a relaunch mid-setup
            // costs one click rather than three.
            <WelcomeStep onNext={() => setStep(firstUnsatisfied(readiness))} />
          )}
          {step === "model" && (
            <ModelStep satisfied={readiness.steps.model.satisfied} onChanged={refresh} />
          )}
          {step === "backend" && (
            <BackendStep satisfied={readiness.steps.backend.satisfied} onChanged={refresh} />
          )}
          {step === "permissions" && (
            <PermissionsStep readiness={readiness.steps.permissions} onChanged={refresh} />
          )}
        </div>

        {error && (
          <div
            role="alert"
            className="mx-auto mt-6 w-full max-w-md rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-[13px] text-amber-900"
          >
            {error}
          </div>
        )}

        {step !== "welcome" && (
          <div className="mx-auto mt-10 flex w-full max-w-md items-center gap-3">
            <button
              type="button"
              onClick={() => setStep(order[Math.max(0, index - 1)])}
              className="flex items-center gap-1.5 rounded-lg px-3 py-3 text-[13px]
                         text-neutral-500 transition hover:text-neutral-900"
            >
              <ArrowLeft size={14} aria-hidden />
              Back
            </button>

            {isLast ? (
              <button
                type="button"
                onClick={() => void finish()}
                disabled={!ready || finishing}
                title={ready ? undefined : "Every step above has to be resolved first"}
                className="flex flex-1 items-center justify-center gap-2 rounded-lg bg-neutral-900
                           px-6 py-3 text-[14px] font-medium text-white transition
                           hover:bg-neutral-800 disabled:cursor-not-allowed disabled:bg-neutral-300"
              >
                {finishing && <Loader2 size={15} className="animate-spin" aria-hidden />}
                Finish setup
              </button>
            ) : (
              <button
                type="button"
                onClick={() => setStep(order[index + 1])}
                className="flex-1 rounded-lg bg-neutral-900 px-6 py-3 text-[14px] font-medium
                           text-white transition hover:bg-neutral-800"
              >
                Continue
              </button>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Write the banner**

Create `apps/desktop/src/onboarding/SetupBanner.tsx`:

```tsx
import { AlertCircle, X } from "lucide-react";

import type { Step } from "./readiness";

interface SetupBannerProps {
  regressed: Step[];
  onDismiss: () => void;
}

/**
 * Something setup established has since broken.
 *
 * A banner rather than the wizard: a stopped Ollama or a revoked grant is a small problem,
 * and demoting an established user to a welcome screen over it would be absurd.
 */
export function SetupBanner({ regressed, onDismiss }: SetupBannerProps) {
  if (regressed.length === 0) return null;

  const names = regressed.map((step) => step.title.toLowerCase()).join(" and ");

  return (
    <div
      role="status"
      className="flex items-center gap-2 border-b border-amber-200 bg-amber-50 px-4 py-2 text-[13px] text-amber-900"
    >
      <AlertCircle size={15} className="shrink-0" aria-hidden />
      <span className="flex-1">
        Recording may not work: {names} {regressed.length === 1 ? "needs" : "need"} attention.
        Open Settings to fix it.
      </span>
      <button
        type="button"
        onClick={onDismiss}
        aria-label="Dismiss"
        className="shrink-0 rounded p-0.5 transition hover:bg-amber-100"
      >
        <X size={14} aria-hidden />
      </button>
    </div>
  );
}
```

- [ ] **Step 3: Write the gate**

Create `apps/desktop/src/onboarding/SetupGate.tsx`:

```tsx
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Loader2 } from "lucide-react";

import { api } from "../lib/api";
import { regressions, type SetupReadiness } from "./readiness";
import { SetupBanner } from "./SetupBanner";
import { SetupFlow } from "./SetupFlow";

interface SetupGateProps {
  children: ReactNode;
}

/**
 * Decides whether this launch shows the wizard or the app.
 *
 * Readiness comes from the engine, never from browser storage: the shell binds port 0, so
 * the window's origin changes every launch and anything kept in `localStorage` would be gone
 * by the next one.
 */
export function SetupGate({ children }: SetupGateProps) {
  const [readiness, setReadiness] = useState<SetupReadiness | null>(null);
  const [loading, setLoading] = useState(true);
  const [dismissed, setDismissed] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setReadiness(await api.setup());
    } catch {
      // An unreachable engine is not an unfinished setup, and blocking the app behind a
      // wizard we cannot populate would strand the user. Let the app load and report the
      // failure through its own error banner.
      setReadiness(null);
    }
  }, []);

  useEffect(() => {
    void refresh().finally(() => setLoading(false));
  }, [refresh]);

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center bg-neutral-50">
        <Loader2 size={20} className="animate-spin text-neutral-400" aria-label="Loading" />
      </div>
    );
  }

  if (readiness && readiness.completed_at === null) {
    return (
      <SetupFlow
        readiness={readiness}
        refresh={refresh}
        onFinished={() => void refresh()}
      />
    );
  }

  const regressed = readiness && !dismissed ? regressions(readiness) : [];

  return (
    <div className="flex h-full flex-col">
      <SetupBanner regressed={regressed} onDismiss={() => setDismissed(true)} />
      <div className="min-h-0 flex-1">{children}</div>
    </div>
  );
}
```

- [ ] **Step 4: Wire it into App.tsx**

In `apps/desktop/src/App.tsx`, add to the imports after the views:

```tsx
import { SetupGate } from "./onboarding/SetupGate";
```

Change line 296-298 from:

```tsx
  return (
    <div className="flex h-full overflow-hidden">
      <Sidebar view={view} onChange={setView} isRecording={isRecording} />
```

to:

```tsx
  return (
    <SetupGate>
      <div className="flex h-full overflow-hidden">
        <Sidebar view={view} onChange={setView} isRecording={isRecording} />
```

and the closing tags at lines 406-408 from:

```tsx
      </div>
    </div>
  );
}
```

to:

```tsx
        </div>
      </div>
    </SetupGate>
  );
}
```

Re-indent the JSX between them by one level so Prettier and the reader agree.

- [ ] **Step 5: Verify it compiles and tests pass**

```bash
cd apps/desktop && npx tsc --noEmit && npm test
```

Expected: no type errors, 11 tests pass.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/src/onboarding apps/desktop/src/App.tsx
git commit -m "feat(desktop): gate first launch behind setup"
```

---

## Task 17: Full verification

- [ ] **Step 1: Engine**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all clean.

- [ ] **Step 2: Frontend**

```bash
cd apps/desktop && npm run build && npm test
```

Expected: `tsc --noEmit` passes, Vite build succeeds, 11 tests pass.

- [ ] **Step 3: Run the wizard against a real engine**

```bash
cargo run -p notewise-cli --features full -- serve
```

Open the printed URL. Confirm, in order:

1. Welcome appears; Continue lands on Model (not Backend) when no model is installed.
2. The download bar advances and the row flips to "Installed".
3. Backend shows a red dot with Ollama stopped, green after `ollama serve`, and Re-check flips it without a reload.
4. System audio reads "Not available" with a reason, and does **not** block Finish.
5. Finish is disabled until microphone is granted, then enables.
6. Restart the engine — the app opens straight to the main window, no wizard.

- [ ] **Step 4: Confirm the gate cannot be bypassed**

With a fresh database:

```bash
curl -s -X POST http://127.0.0.1:<port>/v1/setup/complete | jq
```

Expected: HTTP 409 and an error naming `model`.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "chore: verify first-run setup end to end" || echo "nothing to commit"
```

---

## Self-Review

**Spec coverage**

| Spec section | Task |
|---|---|
| §2 R1 four steps | 12, 13, 14, 15, 16 |
| §2 R2 every step required | 5 (`unsatisfied`), 7 (409), 9 (`canFinish`) |
| §2.1 / R3 required only when available | 3, 5, 6, 9 |
| §2 R4 detect, don't download | 2, 14 |
| §2 R5 survives restart | 1, 6, 7 |
| §2 R6 banner not wizard replay | 16 |
| §4.1 one persisted value, Welcome entry rule | 1, 9, 16 |
| §4.3 `app_settings` + repository | 1 |
| §5.1 `GET /v1/setup` | 6 |
| §5.2 `POST /v1/setup/complete` | 7 |
| §5.3 `POST /v1/permissions/:kind` | 7 |
| §6 permission probing | 3 |
| §7 backend detection | 2 |
| §8.1 structure, App.tsx unchanged in substance | 16 |
| §8.2 the four screens | 12–15 |
| §8.3 post-completion banner | 16 |
| §9 `list_models` model-dir fix | 4 |
| §9 duplicate `size()` and download logic | 10 |
| §10.1 Rust tests | 1, 2, 3, 4, 5, 6, 7 |
| §10.2 Vitest on `readiness.ts` | 9 |

No spec requirement is unassigned.

**Placeholder scan:** none. Every code step carries complete code; every command carries its
expected result.

**Type consistency:** `PermissionStatus` (Rust) serialises to the four strings typed as
`PermissionState` in `readiness.ts`. `SetupReadiness`, `StepReadiness`,
`PermissionsReadiness`, and `PermissionReadiness` have matching field names and
`snake_case` on both sides — `system_audio`, `completed_at`, `downloaded_bytes`. The
audio-capture functions are exported as `permission_status` / `request_permission` in Task 3
and called by those names in Task 6. `useModelDownload` returns `{ downloading, progress,
error, start, clearError }` in Task 10 and is destructured to that shape in Tasks 10 and 13.
