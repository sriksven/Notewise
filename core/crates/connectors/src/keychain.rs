//! `CredentialStore` over the platform keychain.
//!
//! macOS Keychain, Windows Credential Manager, and Secret Service on Linux, via the
//! `keyring` crate. A missing entry is `Ok(None)` rather than an error — "not connected yet"
//! is the normal state for most connectors, not a failure.

use keyring::Entry;

use crate::credentials::{CredentialStore, Secret};
use crate::error::{ConnectorError, Result};

#[derive(Debug, Default)]
pub struct KeychainStore;

impl KeychainStore {
    pub fn new() -> Self {
        Self
    }

    /// The keychain service name for a connector.
    ///
    /// Namespaced so that two connectors storing a key called `"token"` cannot collide, and
    /// so a user auditing their keychain can see which entry belongs to what.
    pub fn service_name(&self, connector_id: &str) -> String {
        format!("com.notewise.connector.{connector_id}")
    }

    fn entry(&self, connector_id: &str, key: &str) -> Result<Entry> {
        Entry::new(&self.service_name(connector_id), key)
            .map_err(|e| ConnectorError::Credential(e.to_string()))
    }
}

impl CredentialStore for KeychainStore {
    fn get(&self, connector_id: &str, key: &str) -> Result<Option<Secret>> {
        match self.entry(connector_id, key)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(ConnectorError::Credential(e.to_string())),
        }
    }

    fn set(&self, connector_id: &str, key: &str, value: &Secret) -> Result<()> {
        self.entry(connector_id, key)?
            .set_password(value.expose())
            .map_err(|e| ConnectorError::Credential(e.to_string()))
    }

    fn delete(&self, connector_id: &str, key: &str) -> Result<()> {
        match self.entry(connector_id, key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(ConnectorError::Credential(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_name_is_namespaced_per_connector() {
        let store = KeychainStore::new();
        assert_eq!(
            store.service_name("linear"),
            "com.notewise.connector.linear"
        );
        assert_ne!(store.service_name("linear"), store.service_name("jira"));
    }

    #[test]
    #[ignore = "needs an unlocked OS keychain; CI has no login session"]
    fn round_trips_through_the_real_keychain() {
        let store = KeychainStore::new();
        let secret = Secret::new("integration-test-value");

        store.set("notewise_test", "token", &secret).unwrap();
        let found = store.get("notewise_test", "token").unwrap();
        assert_eq!(
            found.map(|s| s.expose().to_string()),
            Some("integration-test-value".into())
        );

        store.delete("notewise_test", "token").unwrap();
        assert!(store.get("notewise_test", "token").unwrap().is_none());
    }
}
