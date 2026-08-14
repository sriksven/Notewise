//! Credential storage.
//!
//! A long-lived refresh token has a different risk profile from a meeting summary: it grants
//! standing access to someone's calendar or tracker, and it is exactly the kind of value that
//! ends up inside a support bundle. So credentials do not go in the database — they go behind
//! this trait, whose production implementation is the OS keychain.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::Result;

/// A credential value that does not print itself.
///
/// `Debug` is implemented by hand precisely so an ordinary `{:?}` on a struct holding one
/// cannot leak it into a log line.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// Read the underlying value. Named to make call sites conspicuous in review.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(redacted)")
    }
}

/// Where connector credentials live.
///
/// Keys are namespaced by `connector_id` so two connectors cannot collide on `"token"`.
pub trait CredentialStore: Send + Sync + std::fmt::Debug {
    fn get(&self, connector_id: &str, key: &str) -> Result<Option<Secret>>;
    fn set(&self, connector_id: &str, key: &str, value: &Secret) -> Result<()>;
    /// Remove a credential. Removing an absent credential succeeds.
    fn delete(&self, connector_id: &str, key: &str) -> Result<()>;
}

/// An in-process credential store for tests.
///
/// Exists so credential-handling logic is testable on a CI machine with no unlocked keychain.
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<(String, String), Secret>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemoryStore {
    fn get(&self, connector_id: &str, key: &str) -> Result<Option<Secret>> {
        let entries = self.entries.lock().expect("credential mutex poisoned");
        Ok(entries
            .get(&(connector_id.to_string(), key.to_string()))
            .cloned())
    }

    fn set(&self, connector_id: &str, key: &str, value: &Secret) -> Result<()> {
        let mut entries = self.entries.lock().expect("credential mutex poisoned");
        entries.insert((connector_id.to_string(), key.to_string()), value.clone());
        Ok(())
    }

    fn delete(&self, connector_id: &str, key: &str) -> Result<()> {
        let mut entries = self.entries.lock().expect("credential mutex poisoned");
        entries.remove(&(connector_id.to_string(), key.to_string()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_the_secret() {
        let secret = Secret::new("ya29.super-secret-refresh-token");

        let rendered = format!("{secret:?}");
        assert!(
            !rendered.contains("ya29"),
            "a token must not reach a log through {{:?}}, got {rendered}"
        );
        assert_eq!(rendered, "Secret(redacted)");
    }

    #[test]
    fn expose_returns_the_real_value() {
        let secret = Secret::new("hunter2");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn memory_store_round_trips() {
        let store = MemoryStore::new();
        store
            .set("google_calendar", "refresh_token", &Secret::new("abc"))
            .unwrap();

        let found = store.get("google_calendar", "refresh_token").unwrap();
        assert_eq!(found.map(|s| s.expose().to_string()), Some("abc".into()));
    }

    #[test]
    fn credentials_are_namespaced_by_connector() {
        let store = MemoryStore::new();
        store.set("linear", "token", &Secret::new("l")).unwrap();
        store.set("jira", "token", &Secret::new("j")).unwrap();

        assert_eq!(
            store
                .get("linear", "token")
                .unwrap()
                .map(|s| s.expose().to_string()),
            Some("l".into())
        );
    }

    #[test]
    fn delete_removes_and_absent_delete_succeeds() {
        let store = MemoryStore::new();
        store.set("vault", "hmac", &Secret::new("k")).unwrap();

        store.delete("vault", "hmac").unwrap();
        assert!(store.get("vault", "hmac").unwrap().is_none());
        assert!(store.delete("vault", "hmac").is_ok());
    }
}
