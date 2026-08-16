use std::path::{Path, PathBuf};

use std::sync::Arc;

use notewise_ai_router::{BackendKind, Router as AiRouter, RouterConfig};
use notewise_storage::{Database, SettingsRepository};
use tokio::sync::Mutex;

use crate::downloads::DownloadManager;
use crate::recording::RecordingManager;

/// Shared application state.
///
/// The database is behind a mutex because `rusqlite::Connection` is `Send` but not `Sync`.
/// This is a real serialization point: SQLite writes are serialized anyway, and the
/// alternative — a connection pool — buys nothing for a single-user local engine while adding
/// a WAL-writer-contention failure mode.
///
/// It is a `tokio::sync::Mutex` rather than a `std` one so a handler that awaits mid-request
/// cannot block a runtime worker thread.
#[derive(Debug)]
pub struct AppState {
    db: Mutex<Database>,
    /// The database file, remembered so the recording pipeline can open its own connection
    /// instead of holding this mutex for the length of a meeting. `None` when in-memory.
    db_path: Option<PathBuf>,
    model_dir: PathBuf,
    /// Swappable at runtime, so a user can change backend without restarting the app.
    ///
    /// An `RwLock<Arc<_>>` rather than an `RwLock<AiRouter>`: handlers clone the `Arc` and drop
    /// the lock immediately, so a summarization that takes thirty seconds does not block
    /// someone switching model in another window.
    ai: std::sync::RwLock<Arc<AiRouter>>,
    /// Connectors currently registered.
    ///
    /// An `RwLock<Arc<_>>` for the same reason as `ai`: connecting a vault in one window must
    /// not block a request reading the list in another. Handlers clone the `Arc` and drop the
    /// lock immediately.
    connectors: std::sync::RwLock<Arc<notewise_connectors::ConnectorRegistry>>,
    recording: RecordingManager,
    downloads: DownloadManager,
    /// Agent runs, in memory. See [`crate::agent`] for why they are not persisted.
    agents: crate::agent::AgentRegistry,
    /// Speaker events posted for meetings that have not ended yet.
    ///
    /// Not on [`RecordingManager`]: that is compiled out entirely in a build without capture, and
    /// the extension reports who was speaking whether or not this engine recorded the audio. An
    /// entry is drained when its meeting ends — see
    /// [`crate::speakers::apply_pending_timeline`].
    speaker_timelines: crate::speakers::PendingTimelines,
}

impl AppState {
    pub fn new(db: Database, ai: AiRouter) -> Self {
        let db_path = db.path().map(Path::to_path_buf);
        Self {
            db: Mutex::new(db),
            db_path,
            model_dir: default_model_dir(),
            ai: std::sync::RwLock::new(Arc::new(ai)),
            connectors: std::sync::RwLock::new(Arc::new(
                notewise_connectors::ConnectorRegistry::new(),
            )),
            recording: RecordingManager::new(),
            downloads: DownloadManager::new(),
            agents: crate::agent::AgentRegistry::new(),
            speaker_timelines: Default::default(),
        }
    }

    /// Override where transcription models are looked up.
    ///
    /// The desktop app keeps them beside the database inside its app-data container; the CLI
    /// honours `NOTEWISE_MODEL_DIR`. Neither should have to match a hard-coded guess in here.
    pub fn with_model_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.model_dir = dir.into();
        self
    }

    /// The database file, or `None` for an in-memory engine.
    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }

    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }

    /// The at-most-one live recording.
    pub fn recording(&self) -> &RecordingManager {
        &self.recording
    }

    /// Model downloads, running and finished.
    pub fn downloads(&self) -> &DownloadManager {
        &self.downloads
    }

    /// Agent runs, running and finished.
    pub fn agents(&self) -> &crate::agent::AgentRegistry {
        &self.agents
    }

    /// Speaker events accumulated for meetings still in progress.
    pub fn speaker_timelines(&self) -> &crate::speakers::PendingTimelines {
        &self.speaker_timelines
    }

    /// The model store this engine reads and writes.
    ///
    /// Built from [`Self::model_dir`] rather than re-resolving the environment, so a download
    /// started through the API lands where recording will look for it.
    pub fn model_store(&self) -> notewise_transcription::ModelStore {
        notewise_transcription::ModelStore::new(&self.model_dir)
    }

    /// Lock the database.
    ///
    /// Hold the guard for as little as possible, and never across an `.await` on a model
    /// call — summarization can take tens of seconds and would stall every other request.
    pub async fn db(&self) -> tokio::sync::MutexGuard<'_, Database> {
        self.db.lock().await
    }

    /// The active AI router.
    ///
    /// Returns an owned handle rather than a borrow, so the lock is released before a caller
    /// spends thirty seconds inside a model call.
    pub fn ai(&self) -> Arc<AiRouter> {
        Arc::clone(&self.ai.read().expect("ai router lock poisoned"))
    }

    /// The connectors this engine can deliver through.
    ///
    /// Returns an owned handle for the same reason as [`Self::ai`]: the lock is released before
    /// the caller does anything with the registry.
    pub fn connectors(&self) -> Arc<notewise_connectors::ConnectorRegistry> {
        Arc::clone(
            &self
                .connectors
                .read()
                .expect("connector registry lock poisoned"),
        )
    }

    /// Replace the registry — used when a connector is connected or disconnected.
    pub fn set_connectors(&self, registry: notewise_connectors::ConnectorRegistry) {
        *self
            .connectors
            .write()
            .expect("connector registry lock poisoned") = Arc::new(registry);
    }

    /// Replace the active backend.
    ///
    /// Built and validated *before* the swap, so a backend that cannot be constructed leaves
    /// the working one in place. Half-switching would leave the app with no usable model and
    /// no obvious way back.
    pub async fn switch_backend(
        &self,
        kind: BackendKind,
        model: Option<String>,
        endpoint: Option<String>,
    ) -> Result<(), notewise_ai_router::AiError> {
        let mut config = RouterConfig::new(kind);

        // Read from the environment, never from the request: a key must not travel over HTTP
        // or land in a log, even on loopback.
        if let Some(key) = api_key_for(kind) {
            config = config.with_api_key(key);
        }
        if let Some(model) = model {
            config = config.with_model(model);
        }
        if let Some(endpoint) = endpoint {
            config = config.with_endpoint(endpoint);
        }

        let router = Arc::new(AiRouter::from_config(config)?);
        let model_id = router.model_id().to_string();
        *self.ai.write().expect("ai router lock poisoned") = router;

        // Remember it. Without this the choice lasts until the process exits, and the engine
        // comes back on its inferred default — which is how a user who picked `llama3.1:8b`
        // gets "model 'llama3.1' not found" on the next launch, having already fixed it once.
        //
        // Best effort: the switch itself has succeeded and the user is using the new backend.
        // Failing the request because the preference could not be written would undo a change
        // that already works.
        {
            let db = self.db().await;
            let settings = SettingsRepository::new(&db);
            if let Err(e) = settings
                .set(BACKEND_KIND_KEY, kind.as_str())
                .and_then(|()| settings.set(BACKEND_MODEL_KEY, &model_id))
            {
                tracing::warn!(error = %e, "could not persist the backend choice");
            }
        }

        Ok(())
    }
}

/// Where the chosen backend is remembered, alongside `onboarding_completed_at`.
pub const BACKEND_KIND_KEY: &str = "ai_backend_kind";
pub const BACKEND_MODEL_KEY: &str = "ai_backend_model";

/// The API key for a backend, from the environment.
pub(crate) fn api_key_for(kind: BackendKind) -> Option<String> {
    // The keychain first. A key the user typed into the app is a deliberate, current choice;
    // a stale variable left in a shell profile should not silently win over it.
    if let Some(secret) = keychain_key(kind) {
        return Some(secret);
    }
    env_key(kind)
}

/// The environment variable each provider conventionally uses.
pub(crate) fn env_key(kind: BackendKind) -> Option<String> {
    let name = match kind {
        BackendKind::Anthropic => "ANTHROPIC_API_KEY",
        BackendKind::Gemini => "GEMINI_API_KEY",
        BackendKind::Groq => "GROQ_API_KEY",
        BackendKind::OpenRouter => "OPENROUTER_API_KEY",
        _ => return None,
    };
    std::env::var(name).ok().filter(|k| !k.trim().is_empty())
}

/// The credential-store entry name for a backend's key.
///
/// Namespaced away from connector ids so a provider and a connector that happen to share a name
/// cannot read each other's secrets.
pub(crate) fn key_entry(kind: BackendKind) -> String {
    format!("backend:{}", kind.as_str())
}

/// The key a user saved through the app, from the OS keychain.
///
/// The keychain, never the database: the database is a plain SQLite file that gets copied into
/// backups and support bundles, and a provider key sitting in it would go with them.
fn keychain_key(kind: BackendKind) -> Option<String> {
    use notewise_connectors::CredentialStore;

    notewise_connectors::KeychainStore::new()
        .get(&key_entry(kind), API_KEY_FIELD)
        .ok()
        .flatten()
        .map(|s| s.expose().to_string())
        .filter(|k| !k.trim().is_empty())
}

/// The field name under which a provider key is stored.
pub const API_KEY_FIELD: &str = "api_key";

/// Where transcription models live by default.
///
/// Mirrors the CLI's resolution so `notewise serve` and the desktop app find the same models a
/// `notewise record` run downloaded — a user who has already waited for a 148 MB download must
/// not be asked to do it again by a different front end.
fn default_model_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("NOTEWISE_MODEL_DIR") {
        return PathBuf::from(dir);
    }

    let base = match std::env::var("NOTEWISE_DATA_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".into());
            let home = PathBuf::from(home);
            if cfg!(target_os = "macos") {
                home.join("Library/Application Support/notewise")
            } else if cfg!(target_os = "windows") {
                home.join("AppData/Roaming/notewise")
            } else {
                home.join(".local/share/notewise")
            }
        }
    };
    base.join("models")
}

#[cfg(test)]
mod tests {
    use super::*;
    use notewise_ai_router::{Router, RouterConfig};

    fn state() -> AppState {
        AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            Router::from_config(RouterConfig::mock()).expect("mock router"),
        )
    }

    #[tokio::test]
    async fn database_is_reachable_through_the_lock() {
        let state = state();
        let db = state.db().await;
        assert_eq!(
            db.schema_version().unwrap(),
            notewise_storage::SUPPORTED_VERSION
        );
    }

    #[tokio::test]
    async fn the_lock_is_released_between_uses() {
        let state = state();
        {
            let _guard = state.db().await;
        }
        // Would deadlock if the first guard were still held.
        let _second = state.db().await;
    }

    #[tokio::test]
    async fn an_in_memory_database_reports_no_path() {
        // Recording keys off this: a second connection to `:memory:` would be a different,
        // empty database, so the absence must be visible rather than guessed.
        assert_eq!(state().db_path(), None);
    }

    #[test]
    fn a_file_backed_database_reports_its_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notewise.db");
        let state = AppState::new(
            Database::open(&path).expect("db"),
            Router::from_config(RouterConfig::mock()).expect("router"),
        );
        assert_eq!(state.db_path(), Some(path.as_path()));
    }

    #[test]
    fn the_model_directory_can_be_overridden() {
        let state = state().with_model_dir("/models");
        assert_eq!(state.model_dir(), Path::new("/models"));
    }

    #[tokio::test]
    async fn no_recording_is_active_on_a_fresh_state() {
        assert!(state().recording().status().await.is_none());
    }

    #[tokio::test]
    async fn ai_is_reachable_without_taking_the_database_lock() {
        let state = state();
        let _db_guard = state.db().await;

        // Model calls must not be serialized behind database access.
        assert_eq!(state.ai().model_id(), "mock");
    }
}
