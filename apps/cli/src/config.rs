//! Where the database lives and which AI backend to use.

use std::fmt;
use std::path::PathBuf;

use anyhow::Result;
use notewise_ai_router::{Router as AiRouter, RouterConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseLocation {
    /// Throwaway, nothing persisted.
    Memory,
    File(PathBuf),
}

impl fmt::Display for DatabaseLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DatabaseLocation::Memory => write!(f, "(in-memory, nothing persisted)"),
            DatabaseLocation::File(path) => write!(f, "{}", path.display()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub database: DatabaseLocation,
    backend: RouterConfig,
}

impl Config {
    /// Resolve configuration from flags and the environment.
    ///
    /// The AI backend is chosen by which credentials are present, and **defaults to local**.
    /// A user who has set nothing gets local inference, not an accidental upload of their
    /// meetings to a third party.
    pub fn resolve(db: Option<PathBuf>, ephemeral: bool) -> Result<Self> {
        let database = if ephemeral {
            DatabaseLocation::Memory
        } else {
            DatabaseLocation::File(match db {
                Some(path) => path,
                None => default_database_path()?,
            })
        };

        Ok(Self {
            database,
            backend: backend_from_env(),
        })
    }

    pub fn ai_router(&self) -> Result<AiRouter> {
        Ok(AiRouter::from_config(self.backend.clone())?)
    }
}

/// Choose a backend from the environment.
///
/// Order matters: an explicit API key is an opt-in to remote inference, so it wins. With
/// nothing set, the answer is local.
fn backend_from_env() -> RouterConfig {
    // Explicit override, for demos, CI, and UI development with no model installed.
    if std::env::var("NOTEWISE_BACKEND").as_deref() == Ok("mock") {
        return RouterConfig::mock();
    }

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.trim().is_empty() {
            let mut config = RouterConfig::anthropic(key);
            if let Ok(model) = std::env::var("NOTEWISE_MODEL") {
                config = config.with_model(model);
            }
            return config;
        }
    }

    let mut config = RouterConfig::ollama();
    if let Ok(model) = std::env::var("NOTEWISE_MODEL") {
        config = config.with_model(model);
    }
    if let Ok(host) = std::env::var("OLLAMA_HOST") {
        config = config.with_endpoint(format!("{}/api/chat", host.trim_end_matches('/')));
    }
    config
}

/// Default database path, following each platform's convention.
fn default_database_path() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("NOTEWISE_DATA_DIR") {
        return Ok(PathBuf::from(dir).join("notewise.db"));
    }

    let base = if cfg!(target_os = "macos") {
        home()?.join("Library/Application Support")
    } else if cfg!(target_os = "windows") {
        std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or(home()?.join("AppData/Roaming"))
    } else {
        // XDG: honour the override before falling back to the spec's default.
        std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or(home()?.join(".local/share"))
    };

    Ok(base.join("notewise").join("notewise.db"))
}

fn home() -> Result<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .map_err(|_| anyhow::anyhow!("could not determine the home directory; pass --db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ephemeral_uses_memory() {
        let config = Config::resolve(None, true).unwrap();
        assert_eq!(config.database, DatabaseLocation::Memory);
    }

    #[test]
    fn an_explicit_path_is_honoured() {
        let config = Config::resolve(Some(PathBuf::from("/tmp/custom.db")), false).unwrap();
        assert_eq!(
            config.database,
            DatabaseLocation::File(PathBuf::from("/tmp/custom.db"))
        );
    }

    #[test]
    fn ephemeral_overrides_an_explicit_path() {
        let config = Config::resolve(Some(PathBuf::from("/tmp/custom.db")), true).unwrap();
        assert_eq!(config.database, DatabaseLocation::Memory);
    }

    #[test]
    fn the_default_path_lands_under_a_notewise_directory() {
        let path = default_database_path().unwrap();
        assert!(path.ends_with("notewise/notewise.db"), "{}", path.display());
    }

    #[test]
    fn memory_location_says_nothing_is_persisted() {
        // Shown by `notewise status`; a user must not think ephemeral data is being kept.
        let shown = DatabaseLocation::Memory.to_string();
        assert!(shown.contains("nothing persisted"), "{shown}");
    }

    #[test]
    fn an_ephemeral_database_is_usable() {
        let config = Config::resolve(None, true).unwrap();
        assert!(config.ai_router().is_ok());
    }
}
