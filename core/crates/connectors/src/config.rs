//! Building a registry from what the user has actually connected.
//!
//! The registry is derived state: `connector_accounts` plus the keychain are the source of
//! truth, so a restart rebuilds exactly what was configured, and a connector missing its
//! credential is simply absent rather than half-working.

use std::sync::Arc;

use notewise_storage::{AccountStatus, ConnectorAccountRepository, Database};
use uuid::Uuid;

use crate::credentials::{CredentialStore, Secret};
use crate::error::Result;
use crate::registry::ConnectorRegistry;
use crate::sinks::{VaultSink, WebhookSink};

/// Credential key holding a webhook's HMAC signing secret.
pub const SIGNING_KEY: &str = "signing_secret";

/// A fresh shared secret for signing webhook deliveries.
///
/// Two v4 UUIDs, hex, for 244 bits of entropy — well past what an HMAC key needs, and it
/// avoids taking a dependency on an RNG crate for one call site.
pub fn generate_signing_secret() -> Secret {
    Secret::new(format!(
        "{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    ))
}

/// Construct the registry described by the database and credential store.
///
/// A connector whose configuration is incomplete is skipped, not registered in a degraded
/// state. Half a connector is worse than none: it fails at delivery time, in the background,
/// where the user is least likely to see why.
pub fn build_registry(
    db: &Database,
    credentials: &dyn CredentialStore,
) -> Result<ConnectorRegistry> {
    let mut registry = ConnectorRegistry::new();

    for account in ConnectorAccountRepository::new(db).list()? {
        if account.status != AccountStatus::Connected {
            continue;
        }

        let Some(target) = account.account_label.as_deref() else {
            tracing::warn!(connector = %account.connector_id, "connected with no target; skipping");
            continue;
        };

        match account.connector_id.as_str() {
            "vault" => registry.register_sink(Arc::new(VaultSink::new(target))),
            "webhook" => match credentials.get("webhook", SIGNING_KEY)? {
                Some(secret) => registry.register_sink(Arc::new(WebhookSink::new(target, secret))),
                None => tracing::warn!("webhook has no signing secret; skipping"),
            },
            other => tracing::warn!(connector = %other, "no such connector in this build"),
        }
    }

    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::MemoryStore;
    use notewise_storage::ConnectorAccountRepository;

    #[test]
    fn an_empty_database_registers_nothing() {
        let db = Database::open_in_memory().unwrap();
        let registry = build_registry(&db, &MemoryStore::new()).unwrap();
        assert!(registry.sink_ids().is_empty());
    }

    #[test]
    fn a_connected_vault_is_registered() {
        let db = Database::open_in_memory().unwrap();
        ConnectorAccountRepository::new(&db)
            .connect("vault", Some("/tmp/notes"), &[])
            .unwrap();

        let registry = build_registry(&db, &MemoryStore::new()).unwrap();
        assert_eq!(registry.sink_ids(), vec!["vault".to_string()]);
    }

    #[test]
    fn a_connected_webhook_with_a_secret_is_registered() {
        let db = Database::open_in_memory().unwrap();
        ConnectorAccountRepository::new(&db)
            .connect("webhook", Some("https://example.com/hook"), &[])
            .unwrap();
        let store = MemoryStore::new();
        store
            .set("webhook", SIGNING_KEY, &Secret::new("k"))
            .unwrap();

        let registry = build_registry(&db, &store).unwrap();
        assert_eq!(registry.sink_ids(), vec!["webhook".to_string()]);
    }

    #[test]
    fn a_webhook_missing_its_secret_is_not_registered() {
        let db = Database::open_in_memory().unwrap();
        ConnectorAccountRepository::new(&db)
            .connect("webhook", Some("https://example.com/hook"), &[])
            .unwrap();

        let registry = build_registry(&db, &MemoryStore::new()).unwrap();
        assert!(
            registry.sink_ids().is_empty(),
            "signing with an empty key would produce a signature anyone could forge"
        );
    }

    #[test]
    fn a_disabled_account_is_not_registered() {
        let db = Database::open_in_memory().unwrap();
        let accounts = ConnectorAccountRepository::new(&db);
        accounts.connect("vault", Some("/tmp/notes"), &[]).unwrap();
        accounts
            .set_status("vault", AccountStatus::Disabled)
            .unwrap();

        let registry = build_registry(&db, &MemoryStore::new()).unwrap();
        assert!(registry.sink_ids().is_empty());
    }

    #[test]
    fn generated_secrets_are_unique_and_long() {
        let a = generate_signing_secret();
        let b = generate_signing_secret();
        assert_ne!(a.expose(), b.expose());
        assert_eq!(a.expose().len(), 64);
    }
}
