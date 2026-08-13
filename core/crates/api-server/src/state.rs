use std::path::{Path, PathBuf};

use notewise_ai_router::Router as AiRouter;
use notewise_storage::Database;
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
    ai: AiRouter,
    recording: RecordingManager,
    downloads: DownloadManager,
}

impl AppState {
    pub fn new(db: Database, ai: AiRouter) -> Self {
        let db_path = db.path().map(Path::to_path_buf);
        Self {
            db: Mutex::new(db),
            db_path,
            model_dir: default_model_dir(),
            ai,
            recording: RecordingManager::new(),
            downloads: DownloadManager::new(),
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

    /// The AI router. Not behind the database lock, so a slow model call does not block reads.
    pub fn ai(&self) -> &AiRouter {
        &self.ai
    }
}

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
