//! Connector configuration, status, and outbox inspection.
//!
//! The listing side exists so a user can see what is configured and what failed to deliver,
//! because a queue whose failures are invisible is worse than no queue. The connect and
//! disconnect side is what lets them turn a sink on at all: without a folder for the vault or
//! a URL for the webhook there is nothing to deliver to.
//!
//! Configuration is written to `connector_accounts` and the keychain, and the in-memory
//! registry is rebuilt from both afterwards — the registry is derived state, never the record.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use notewise_connectors::{
    build_registry, generate_signing_secret, ConnectorRegistry, CredentialStore, KeychainStore,
    VaultSink, WebhookSink, SIGNING_KEY,
};
use notewise_storage::{ConnectorAccountRepository, Database, OutboxRepository};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ConnectorSummary {
    pub id: String,
    pub display_name: String,
    /// Whether this connector keeps data on the user's machine.
    pub is_local: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct FailedDelivery {
    pub id: String,
    pub connector_id: String,
    pub node_kind: String,
    pub node_id: String,
    pub attempts: u32,
    pub last_error: Option<String>,
}

pub(crate) fn describe_connectors(registry: &ConnectorRegistry) -> Vec<ConnectorSummary> {
    registry
        .sink_ids()
        .into_iter()
        .filter_map(|id| registry.sink(&id).ok())
        .map(|sink| ConnectorSummary {
            id: sink.id().to_string(),
            display_name: sink.display_name().to_string(),
            is_local: sink.is_local(),
        })
        .collect()
}

pub(crate) fn describe_failures(
    db: &Database,
    limit: u32,
) -> notewise_storage::Result<Vec<FailedDelivery>> {
    Ok(OutboxRepository::new(db)
        .list_failed(limit)?
        .into_iter()
        .map(|row| FailedDelivery {
            id: row.id.to_string(),
            connector_id: row.connector_id,
            node_kind: row.node_kind,
            node_id: row.node_id.to_string(),
            attempts: row.attempts,
            last_error: row.last_error,
        })
        .collect())
}

pub async fn list_connectors(State(state): State<Arc<AppState>>) -> Json<Vec<ConnectorSummary>> {
    Json(describe_connectors(&state.connectors()))
}

/// What this build could deliver to, connected or not.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AvailableConnector {
    pub id: &'static str,
    pub display_name: &'static str,
    pub is_local: bool,
    /// What it needs in order to be turned on: a folder, a URL.
    pub target_label: &'static str,
    pub target_hint: &'static str,
    pub description: &'static str,
    pub connected: bool,
}

/// Everything this build can connect to.
///
/// Distinct from [`list_connectors`], which reports what is *configured* — that list is empty
/// on a fresh install and so cannot answer "what could I connect?". Compiled in rather than
/// fetched: this is the set of sinks the binary actually contains, and a catalogue served from
/// elsewhere could offer something this build has no code for.
pub async fn list_available_connectors(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<AvailableConnector>> {
    let registry = state.connectors();
    let connected = registry.sink_ids();
    let is_connected = |id: &str| connected.iter().any(|got| got == id);

    Json(vec![
        AvailableConnector {
            id: VaultSink::ID,
            display_name: "Markdown vault",
            is_local: true,
            target_label: "Folder",
            target_hint: "e.g. ~/Documents/Obsidian/Meetings",
            description: "Writes each meeting to a folder as Markdown. Obsidian, Logseq, or \
                          anything else that reads plain files picks them up.",
            connected: is_connected(VaultSink::ID),
        },
        AvailableConnector {
            id: WebhookSink::ID,
            display_name: "Webhook",
            is_local: false,
            target_label: "URL",
            target_hint: "https://…",
            description: "POSTs each meeting as JSON, signed so the receiver can verify it \
                          came from you. This one leaves your machine.",
            connected: is_connected(WebhookSink::ID),
        },
    ])
}

pub async fn list_failed_deliveries(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<FailedDelivery>>> {
    // `db()` already returns the guard; there is no second `.lock()`. Held across synchronous
    // calls only — `Database` is `Send` but not `Sync`, so keeping it alive across an `.await`
    // would make this future non-`Send` and axum would reject the route.
    let db = state.db().await;
    Ok(Json(describe_failures(&db, 50)?))
}

/// Where this connector should send things: a folder for the vault, a URL for the webhook.
#[derive(Debug, Deserialize)]
pub struct ConnectRequest {
    pub target: String,
}

#[derive(Debug, Serialize)]
pub struct ConnectResponse {
    pub id: String,
    /// Returned exactly once, at connect time. Notewise cannot show it again — it lives in
    /// the OS keychain, and a receiver that loses it must reconnect to get a new one.
    pub signing_secret: Option<String>,
}

pub(crate) fn apply_connect(
    db: &Database,
    credentials: &dyn CredentialStore,
    id: &str,
    request: &ConnectRequest,
) -> ApiResult<ConnectResponse> {
    if request.target.trim().is_empty() {
        return Err(ApiError::BadRequest("target must not be empty".into()));
    }

    // Reject ids this build has no connector for. Without this, POST /v1/connectors/jira
    // succeeds, writes an account row, and then `build_registry` quietly skips it — the
    // "half a connector that fails invisibly" failure this design exists to avoid, moved
    // from the registry to the API boundary.
    if !matches!(id, VaultSink::ID | WebhookSink::ID) {
        return Err(ApiError::NotFound(format!(
            "no connector '{id}' in this build"
        )));
    }

    // Generate a signing secret only when there isn't one. `connect` is an upsert, so
    // "change my webhook URL" is the same call as "connect my webhook" — rotating the key
    // there would silently break every receiver still validating the old one, and since the
    // secret is shown exactly once, a user who changed the URL could not recover it.
    // Rotation should be an explicit act, not a side effect of editing a field.
    let signing_secret = if id == WebhookSink::ID {
        match credentials
            .get(WebhookSink::ID, SIGNING_KEY)
            .map_err(|e| ApiError::Internal(format!("cannot read the signing secret: {e}")))?
        {
            Some(_) => None,
            None => {
                let secret = generate_signing_secret();
                credentials
                    .set(WebhookSink::ID, SIGNING_KEY, &secret)
                    .map_err(|e| {
                        ApiError::Internal(format!("cannot store the signing secret: {e}"))
                    })?;
                Some(secret.expose().to_string())
            }
        }
    } else {
        None
    };

    ConnectorAccountRepository::new(db).connect(id, Some(&request.target), &[])?;

    Ok(ConnectResponse {
        id: id.to_string(),
        signing_secret,
    })
}

pub(crate) fn apply_disconnect(
    db: &Database,
    credentials: &dyn CredentialStore,
    id: &str,
) -> ApiResult<()> {
    // Account row first, then the credential. These live in two stores with no transaction
    // between them, so one of the two failure shapes is going to happen eventually and the
    // question is which one you want.
    //
    // Deleting the credential first leaves an account still marked Connected with no secret,
    // which `build_registry` skips — so the connector goes silently dark while the UI still
    // shows it connected. Deleting the row first leaves an orphaned keychain entry, which is
    // inert: nothing reads a credential except by way of an account row, and the next
    // `connect` overwrites it. `disconnect` on an absent account already succeeds, so
    // retrying finishes the job.
    ConnectorAccountRepository::new(db).disconnect(id)?;
    credentials
        .delete(id, SIGNING_KEY)
        .map_err(|e| ApiError::Internal(format!("cannot remove the credential: {e}")))?;
    Ok(())
}

pub async fn connect_connector(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<ConnectRequest>,
) -> ApiResult<Json<ConnectResponse>> {
    let credentials = KeychainStore::new();
    // Held across synchronous calls only. `Database` is `Send` but not `Sync`, so an `.await`
    // inside this scope would make the future non-`Send` and axum would reject the route.
    let db = state.db().await;

    let response = apply_connect(&db, &credentials, &id, &request)?;
    state.set_connectors(
        build_registry(&db, &credentials)
            .map_err(|e| ApiError::Internal(format!("cannot rebuild connectors: {e}")))?,
    );

    Ok(Json(response))
}

pub async fn disconnect_connector(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    let credentials = KeychainStore::new();
    let db = state.db().await;

    apply_disconnect(&db, &credentials, &id)?;
    state.set_connectors(
        build_registry(&db, &credentials)
            .map_err(|e| ApiError::Internal(format!("cannot rebuild connectors: {e}")))?,
    );

    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notewise_connectors::MockConnector;
    use notewise_storage::{Database, NewOutboxEntry, OutboxRepository};

    #[test]
    fn listing_reports_registered_connectors_and_locality() {
        let mut registry = ConnectorRegistry::new();
        registry.register_sink(std::sync::Arc::new(MockConnector::new("mock")));

        let listed = describe_connectors(&registry);

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "mock");
        assert!(
            listed[0].is_local,
            "the UI shows this, so it must be accurate"
        );
    }

    #[test]
    fn failed_deliveries_are_listable() {
        let db = Database::open_in_memory().unwrap();
        let repo = OutboxRepository::new(&db);
        let row = repo
            .enqueue(NewOutboxEntry {
                connector_id: "webhook".into(),
                node_kind: "decision".into(),
                node_id: notewise_storage::Id::new(),
                operation: "create".into(),
                payload: "{}".into(),
                idempotency_key: "k1".into(),
            })
            .unwrap();
        repo.dead_letter(row.id, "401 unauthorized").unwrap();

        let failures = describe_failures(&db, 10).unwrap();

        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].connector_id, "webhook");
        assert_eq!(failures[0].last_error.as_deref(), Some("401 unauthorized"));
    }

    #[test]
    fn connecting_a_webhook_stores_a_generated_secret() {
        let db = Database::open_in_memory().unwrap();
        let store = notewise_connectors::MemoryStore::new();

        let response = apply_connect(
            &db,
            &store,
            "webhook",
            &ConnectRequest {
                target: "https://example.com/hook".into(),
            },
        )
        .unwrap();

        assert!(
            response.signing_secret.is_some(),
            "the user must be shown it once"
        );
        assert!(
            store
                .get("webhook", notewise_connectors::SIGNING_KEY)
                .unwrap()
                .is_some(),
            "the secret must be persisted or deliveries cannot be signed"
        );
    }

    #[test]
    fn connecting_a_vault_stores_no_secret() {
        let db = Database::open_in_memory().unwrap();
        let store = notewise_connectors::MemoryStore::new();

        let response = apply_connect(
            &db,
            &store,
            "vault",
            &ConnectRequest {
                target: "/tmp/notes".into(),
            },
        )
        .unwrap();

        assert!(response.signing_secret.is_none());
    }

    #[test]
    fn reconnecting_preserves_the_existing_signing_secret() {
        let db = Database::open_in_memory().unwrap();
        let store = notewise_connectors::MemoryStore::new();

        let first = apply_connect(
            &db,
            &store,
            "webhook",
            &ConnectRequest {
                target: "https://a.example/hook".into(),
            },
        )
        .unwrap();
        let issued = first.signing_secret.expect("first connect issues a secret");

        let second = apply_connect(
            &db,
            &store,
            "webhook",
            &ConnectRequest {
                target: "https://b.example/hook".into(),
            },
        )
        .unwrap();

        assert!(
            second.signing_secret.is_none(),
            "changing the URL must not rotate the key"
        );
        assert_eq!(
            store
                .get("webhook", notewise_connectors::SIGNING_KEY)
                .unwrap()
                .map(|s| s.expose().to_string()),
            Some(issued),
            "a receiver validating the old secret must keep working"
        );
    }

    #[test]
    fn an_unknown_connector_id_is_rejected() {
        let db = Database::open_in_memory().unwrap();
        let store = notewise_connectors::MemoryStore::new();

        let result = apply_connect(
            &db,
            &store,
            "jira",
            &ConnectRequest {
                target: "https://jira.example".into(),
            },
        );

        assert!(
            result.is_err(),
            "an id with no connector in this build must not write an account row"
        );
        assert!(
            ConnectorAccountRepository::new(&db)
                .get("jira")
                .unwrap()
                .is_none(),
            "a rejected connect must leave nothing behind"
        );
    }

    #[test]
    fn disconnecting_removes_the_account_and_its_credential() {
        let db = Database::open_in_memory().unwrap();
        let store = notewise_connectors::MemoryStore::new();
        apply_connect(
            &db,
            &store,
            "webhook",
            &ConnectRequest {
                target: "https://x/y".into(),
            },
        )
        .unwrap();

        apply_disconnect(&db, &store, "webhook").unwrap();

        assert!(
            store
                .get("webhook", notewise_connectors::SIGNING_KEY)
                .unwrap()
                .is_none(),
            "a disconnected connector must not leave a live credential behind"
        );
    }
}

#[cfg(test)]
mod route_smoke {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use notewise_ai_router::{Router, RouterConfig};
    use notewise_connectors::{VaultSink, WebhookSink};
    use notewise_storage::Database;
    use tower::ServiceExt;

    async fn get(path: &str) -> (StatusCode, String) {
        let state = crate::AppState::new(
            Database::open_in_memory().unwrap(),
            Router::from_config(RouterConfig::mock()).unwrap(),
        );
        let response = crate::app(std::sync::Arc::new(state))
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn smoke() {
        assert_eq!(
            get("/v1/connectors").await,
            (StatusCode::OK, "[]".to_string())
        );
        assert_eq!(
            get("/v1/connectors/failures").await,
            (StatusCode::OK, "[]".to_string())
        );
    }

    /// The list of what *could* be connected is not the list of what is. On a fresh install
    /// the second is empty, which is exactly when a user needs the first.
    #[tokio::test]
    async fn available_lists_this_build_even_with_nothing_connected() {
        let (status, body) = get("/v1/connectors/available").await;
        assert_eq!(status, StatusCode::OK);

        let available: Vec<serde_json::Value> = serde_json::from_str(&body).expect("json");
        let ids: Vec<_> = available
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();

        assert_eq!(ids, vec![VaultSink::ID, WebhookSink::ID]);
        assert!(available.iter().all(|c| c["connected"] == false));
    }

    /// Whether a sink leaves the machine is the fact a user of a local-first tool needs
    /// before switching it on, so every entry has to carry it.
    #[tokio::test]
    async fn available_says_which_connectors_leave_the_machine() {
        let (_, body) = get("/v1/connectors/available").await;
        let available: Vec<serde_json::Value> = serde_json::from_str(&body).expect("json");

        let vault = available.iter().find(|c| c["id"] == VaultSink::ID).unwrap();
        let webhook = available
            .iter()
            .find(|c| c["id"] == WebhookSink::ID)
            .unwrap();

        assert_eq!(vault["is_local"], true, "a folder is on this machine");
        assert_eq!(webhook["is_local"], false, "a URL is not");
    }

    /// `/available` must not be swallowed by the `/:id` route registered alongside it.
    #[tokio::test]
    async fn available_is_not_shadowed_by_the_id_parameter() {
        let (status, body) = get("/v1/connectors/available").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.starts_with('['), "expected the catalogue, got {body}");
    }
}
