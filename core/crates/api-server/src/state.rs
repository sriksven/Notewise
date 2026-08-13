use notewise_ai_router::Router as AiRouter;
use notewise_storage::Database;
use tokio::sync::Mutex;

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
    ai: AiRouter,
}

impl AppState {
    pub fn new(db: Database, ai: AiRouter) -> Self {
        Self {
            db: Mutex::new(db),
            ai,
        }
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
    async fn ai_is_reachable_without_taking_the_database_lock() {
        let state = state();
        let _db_guard = state.db().await;

        // Model calls must not be serialized behind database access.
        assert_eq!(state.ai().model_id(), "mock");
    }
}
