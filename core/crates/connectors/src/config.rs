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
use crate::sources::{Documents, GoogleBridge, MicrosoftGraph};

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

        // Matched against the sinks' own constants, so a rename cannot leave this arm
        // pointing at a name nothing answers to.
        match account.connector_id.as_str() {
            VaultSink::ID => registry.register_sink(Arc::new(VaultSink::new(target))),
            WebhookSink::ID => match credentials.get(WebhookSink::ID, SIGNING_KEY)? {
                Some(secret) => registry.register_sink(Arc::new(WebhookSink::new(target, secret))),
                None => tracing::warn!("webhook has no signing secret; skipping"),
            },
            // Both a source and a sink: one deployment reads the calendar and creates drafts, so
            // the same handle is registered in both maps. The direction-split traits are what make
            // that expressible rather than needing two connector ids.
            GoogleBridge::ID => {
                let key = credentials.get(GoogleBridge::ID, crate::sources::SHARED_KEY)?;
                match key {
                    Some(key) => {
                        let bridge = Arc::new(GoogleBridge::new(target, key));
                        registry.register_source(bridge);
                    }
                    None => tracing::warn!(
                        "the Google bridge has no shared key; skipping it rather than calling a \
                         deployment that would refuse every request"
                    ),
                }
            }
            MicrosoftGraph::ID => {
                match credentials.get(MicrosoftGraph::ID, crate::sources::REFRESH_TOKEN_KEY)? {
                    Some(token) => {
                        // The account label holds the client id: a tenant that requires its own app
                        // registration supplies one, and otherwise it is the build's own.
                        let graph = Arc::new(MicrosoftGraph::new(target, token));
                        registry.register_source(graph);
                    }
                    None => tracing::warn!(
                        "Microsoft has no refresh token; skipping it rather than registering a \
                         connector that cannot authenticate"
                    ),
                }
            }
            // No credential: a folder on this machine needs a path, which is the account label.
            Documents::ID => registry.register_source(Arc::new(Documents::new(target))),
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
    fn a_connected_google_bridge_with_its_key_becomes_a_source() {
        let db = Database::open_in_memory().unwrap();
        ConnectorAccountRepository::new(&db)
            .connect(
                "google",
                Some("https://script.google.com/macros/s/x/exec"),
                &[],
            )
            .unwrap();
        let store = MemoryStore::new();
        store
            .set("google", crate::sources::SHARED_KEY, &Secret::new("k"))
            .unwrap();

        let registry = build_registry(&db, &store).unwrap();
        assert_eq!(registry.source_ids(), vec!["google".to_string()]);
    }

    #[test]
    fn a_google_bridge_without_its_key_is_not_registered() {
        let db = Database::open_in_memory().unwrap();
        ConnectorAccountRepository::new(&db)
            .connect(
                "google",
                Some("https://script.google.com/macros/s/x/exec"),
                &[],
            )
            .unwrap();

        let registry = build_registry(&db, &MemoryStore::new()).unwrap();
        assert!(
            registry.source_ids().is_empty(),
            "calling a deployment with no key would have every request refused"
        );
    }

    #[test]
    fn a_connected_microsoft_account_with_a_token_becomes_a_source() {
        let db = Database::open_in_memory().unwrap();
        ConnectorAccountRepository::new(&db)
            .connect("microsoft", Some("client-id"), &[])
            .unwrap();
        let store = MemoryStore::new();
        store
            .set(
                "microsoft",
                crate::sources::REFRESH_TOKEN_KEY,
                &Secret::new("refresh"),
            )
            .unwrap();

        let registry = build_registry(&db, &store).unwrap();
        assert_eq!(registry.source_ids(), vec!["microsoft".to_string()]);
    }

    #[test]
    fn a_microsoft_account_without_a_token_is_not_registered() {
        let db = Database::open_in_memory().unwrap();
        ConnectorAccountRepository::new(&db)
            .connect("microsoft", Some("client-id"), &[])
            .unwrap();

        let registry = build_registry(&db, &MemoryStore::new()).unwrap();
        assert!(registry.source_ids().is_empty());
    }

    #[test]
    fn a_connected_folder_becomes_a_source_with_no_credential() {
        let db = Database::open_in_memory().unwrap();
        ConnectorAccountRepository::new(&db)
            .connect("documents", Some("/tmp/vault"), &[])
            .unwrap();

        let registry = build_registry(&db, &MemoryStore::new()).unwrap();
        assert_eq!(
            registry.source_ids(),
            vec!["documents".to_string()],
            "a folder on this machine has nothing to authenticate with"
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
