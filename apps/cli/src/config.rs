//! Where the database lives and which AI backend to use.

use std::fmt;
use std::path::PathBuf;

use anyhow::Result;
use notewise_ai_router::{BackendKind, Router as AiRouter, RouterConfig};

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

    /// The router, preferring a backend the user has already chosen in the app.
    ///
    /// A choice made in the UI used to last until the process exited, because startup only ever
    /// read the environment. Someone who picked `llama3.1:8b` — because `llama3.1` is not what
    /// their Ollama holds — got "model not found" again on the next launch, having already
    /// fixed it once.
    ///
    /// `NOTEWISE_BACKEND` still wins. It is set deliberately for a single launch, and a stored
    /// preference silently overriding it would make the variable look broken.
    pub fn ai_router_with(&self, db: &notewise_storage::Database) -> Result<AiRouter> {
        if std::env::var("NOTEWISE_BACKEND").is_ok() {
            return self.ai_router();
        }

        let settings = notewise_storage::SettingsRepository::new(db);
        let Some(kind) = settings
            .get(notewise_api_server::BACKEND_KIND_KEY)?
            .and_then(|k| BackendKind::parse(k.trim()))
        else {
            return self.ai_router();
        };

        let mut config = RouterConfig::new(kind);
        if kind.requires_api_key() {
            // Still from the environment. A stored preference records *which* provider, never
            // the credential for it.
            if let Some(key) = key_for(kind) {
                config = config.with_api_key(key);
            } else {
                // The key has gone since the choice was made. Falling back beats starting with
                // a backend that will reject every request.
                tracing::warn!(
                    backend = kind.as_str(),
                    "stored backend has no key; ignoring it"
                );
                return self.ai_router();
            }
        }
        if let Some(model) = settings.get(notewise_api_server::BACKEND_MODEL_KEY)? {
            config = config.with_model(model);
        }

        Ok(AiRouter::from_config(config)?)
    }
}

/// Choose a backend from the environment.
///
/// Order matters. An explicit `NOTEWISE_BACKEND` wins, then any API key present (which is an
/// opt-in to remote inference), then local Ollama. With nothing set, the answer is local.
fn backend_from_env() -> RouterConfig {
    let model = std::env::var("NOTEWISE_MODEL").ok();
    let endpoint = std::env::var("NOTEWISE_ENDPOINT").ok();

    let apply = |mut config: RouterConfig| {
        if let Some(model) = model.clone() {
            config = config.with_model(model);
        }
        if let Some(endpoint) = endpoint.clone() {
            config = config.with_endpoint(endpoint);
        }
        config
    };

    // An explicit choice beats key sniffing — a user with several keys set still gets
    // the backend they asked for.
    if let Ok(name) = std::env::var("NOTEWISE_BACKEND") {
        if let Some(kind) = BackendKind::parse(name.trim()) {
            let mut config = RouterConfig::new(kind);
            if kind.requires_api_key() {
                if let Some(key) = key_for(kind) {
                    config = config.with_api_key(key);
                }
            }
            return apply(config);
        }
    }

    // Otherwise infer from whichever key is present.
    for kind in [
        BackendKind::Anthropic,
        BackendKind::Gemini,
        BackendKind::Groq,
        BackendKind::OpenRouter,
    ] {
        if let Some(key) = key_for(kind) {
            return apply(RouterConfig::new(kind).with_api_key(key));
        }
    }

    let mut config = RouterConfig::ollama();
    if let Ok(host) = std::env::var("OLLAMA_HOST") {
        config = config.with_endpoint(format!("{}/api/chat", host.trim_end_matches('/')));
    }
    apply(config)
}

/// The environment variable each provider conventionally uses.
///
/// Reusing the provider's own variable name means a user who already has one exported for
/// another tool does not have to set a Notewise-specific duplicate.
fn key_for(kind: BackendKind) -> Option<String> {
    let name = match kind {
        BackendKind::Anthropic => "ANTHROPIC_API_KEY",
        BackendKind::Gemini => "GEMINI_API_KEY",
        BackendKind::Groq => "GROQ_API_KEY",
        BackendKind::OpenRouter => "OPENROUTER_API_KEY",
        _ => return None,
    };
    std::env::var(name).ok().filter(|k| !k.trim().is_empty())
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
