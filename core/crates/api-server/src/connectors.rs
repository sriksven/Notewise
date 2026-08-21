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
    build_registry, generate_signing_secret, ConnectorRegistry, CredentialStore, GoogleBridge,
    KeychainStore, MicrosoftGraph, Secret, VaultSink, WebhookSink, GOOGLE_SHARED_KEY,
    REFRESH_TOKEN_KEY, SIGNING_KEY,
};

/// The scope name for calendar access.
///
/// Two strings, not an enum: `ConnectorAccount.scopes` is a `Vec<String>` shared by every connector,
/// and an enum here would have to grow a variant for every future capability of every vendor.
pub const CALENDAR_SCOPE: &str = "calendar";
/// The scope name for creating mail drafts. Never for sending — see the sinks.
pub const MAIL_SCOPE: &str = "mail";
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
    /// What this connector points at: a folder, a URL, or a client id.
    pub target: String,
    /// A shared secret the connector needs. The Google bridge's deployment key.
    ///
    /// Goes to the keychain and is never stored in the database or returned.
    #[serde(default)]
    pub key: Option<String>,
    /// Which capabilities to use: `calendar`, `mail`, or both.
    ///
    /// Per-capability opt-in rides here rather than on a second connector, so a user who wants
    /// calendar without mailbox access gets it without a second account row and without a second
    /// credential.
    #[serde(default)]
    pub scopes: Vec<String>,
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
    if !matches!(
        id,
        VaultSink::ID | WebhookSink::ID | GoogleBridge::ID | MicrosoftGraph::ID
    ) {
        return Err(ApiError::NotFound(format!(
            "no connector '{id}' in this build"
        )));
    }

    // The Google bridge is a URL plus a key the user chose when they deployed the script. Without
    // the key `build_registry` skips it, so a connect that stored only the URL would report success
    // and produce a connector that never appears — which is the failure this whole check exists to
    // avoid, one field further in.
    if id == GoogleBridge::ID {
        if !request.target.starts_with("https://script.google.com/")
            && !request.target.starts_with("http://127.0.0.1")
        {
            return Err(ApiError::BadRequest(
                "that is not an Apps Script deployment URL. It should start with \
                 https://script.google.com/"
                    .into(),
            ));
        }

        let key = request
            .key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                ApiError::BadRequest(
                    "the deployment key is required — it is the SHARED_KEY you set in the script"
                        .into(),
                )
            })?;

        credentials
            .set(GoogleBridge::ID, GOOGLE_SHARED_KEY, &Secret::new(key))
            .map_err(|e| ApiError::Internal(format!("cannot store the deployment key: {e}")))?;
    }

    // Microsoft's target is a client id, and the refresh token arrives from the sign-in flow rather
    // than from this call. Connecting without one leaves an account row with no credential, which
    // `build_registry` skips — so it is refused here instead.
    if id == MicrosoftGraph::ID
        && credentials
            .get(MicrosoftGraph::ID, REFRESH_TOKEN_KEY)
            .map_err(|e| ApiError::Internal(format!("cannot read the credential: {e}")))?
            .is_none()
    {
        return Err(ApiError::Conflict(
            "sign in to Microsoft first — connecting only records which account to use".into(),
        ));
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

    // Empty scopes mean calendar only: the capability every account has, and the one that needs no
    // mailbox access. A user has to ask for mail.
    let scopes = if request.scopes.is_empty() && matches!(id, GoogleBridge::ID | MicrosoftGraph::ID)
    {
        vec![CALENDAR_SCOPE.to_string()]
    } else {
        request.scopes.clone()
    };

    ConnectorAccountRepository::new(db).connect(id, Some(&request.target), &scopes)?;

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
                key: None,
                scopes: Vec::new(),
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
                key: None,
                scopes: Vec::new(),
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
                key: None,
                scopes: Vec::new(),
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
                key: None,
                scopes: Vec::new(),
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
                key: None,
                scopes: Vec::new(),
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
                key: None,
                scopes: Vec::new(),
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

#[cfg(test)]
mod setup_tests {
    use super::*;

    // ------------------------------------------------------------ setting the two vendors up

    /// A connect that stored only the URL would report success and produce a connector that never
    /// appears, because `build_registry` skips a bridge with no key.
    #[test]
    fn the_google_bridge_needs_its_deployment_key() {
        let db = Database::open_in_memory().expect("db");
        let credentials = notewise_connectors::MemoryStore::new();

        let refused = apply_connect(
            &db,
            &credentials,
            GoogleBridge::ID,
            &ConnectRequest {
                target: "https://script.google.com/macros/s/AKfy/exec".into(),
                key: None,
                scopes: Vec::new(),
            },
        );

        assert!(
            matches!(refused, Err(ApiError::BadRequest(ref m)) if m.contains("deployment key")),
            "{refused:?}"
        );
        assert!(
            ConnectorAccountRepository::new(&db)
                .list()
                .expect("reads")
                .is_empty(),
            "nothing should have been recorded"
        );
    }

    /// A URL that is not an Apps Script deployment is a paste of the wrong thing, and the error
    /// should say which thing.
    #[test]
    fn a_url_that_is_not_a_deployment_is_refused() {
        let db = Database::open_in_memory().expect("db");
        let credentials = notewise_connectors::MemoryStore::new();

        let refused = apply_connect(
            &db,
            &credentials,
            GoogleBridge::ID,
            &ConnectRequest {
                target: "https://docs.google.com/spreadsheets/d/abc".into(),
                key: Some("k".into()),
                scopes: Vec::new(),
            },
        );
        assert!(
            matches!(refused, Err(ApiError::BadRequest(ref m)) if m.contains("script.google.com")),
            "{refused:?}"
        );
    }

    #[test]
    fn connecting_the_google_bridge_stores_the_key_in_the_keychain_and_not_the_database() {
        let db = Database::open_in_memory().expect("db");
        let credentials = notewise_connectors::MemoryStore::new();

        apply_connect(
            &db,
            &credentials,
            GoogleBridge::ID,
            &ConnectRequest {
                target: "https://script.google.com/macros/s/AKfy/exec".into(),
                key: Some("chosen-at-deploy-time".into()),
                scopes: vec![CALENDAR_SCOPE.into(), MAIL_SCOPE.into()],
            },
        )
        .expect("connects");

        let stored = credentials
            .get(GoogleBridge::ID, GOOGLE_SHARED_KEY)
            .expect("reads")
            .expect("the key is in the keychain");
        assert_eq!(stored.expose(), "chosen-at-deploy-time");

        let account = ConnectorAccountRepository::new(&db)
            .list()
            .expect("reads")
            .into_iter()
            .next()
            .expect("an account");
        assert_eq!(account.connector_id, GoogleBridge::ID);
        assert_eq!(
            account.account_label.as_deref(),
            Some("https://script.google.com/macros/s/AKfy/exec")
        );
        assert_eq!(
            account.scopes,
            vec!["calendar".to_string(), "mail".to_string()]
        );
    }

    /// Mail is opt-in. An account connected without asking gets calendar only, which is the
    /// capability that needs no mailbox access.
    #[test]
    fn connecting_without_asking_for_scopes_gets_calendar_only() {
        let db = Database::open_in_memory().expect("db");
        let credentials = notewise_connectors::MemoryStore::new();

        apply_connect(
            &db,
            &credentials,
            GoogleBridge::ID,
            &ConnectRequest {
                target: "https://script.google.com/macros/s/AKfy/exec".into(),
                key: Some("k".into()),
                scopes: Vec::new(),
            },
        )
        .expect("connects");

        let account = ConnectorAccountRepository::new(&db)
            .list()
            .expect("reads")
            .into_iter()
            .next()
            .expect("an account");
        assert_eq!(account.scopes, vec![CALENDAR_SCOPE.to_string()]);
        assert!(!account.scopes.iter().any(|s| s == MAIL_SCOPE));
    }

    /// Connecting Microsoft without a token would leave an account row with no credential, which
    /// `build_registry` skips — a connector that reports connected and does nothing.
    #[test]
    fn microsoft_cannot_be_connected_before_signing_in() {
        let db = Database::open_in_memory().expect("db");
        let credentials = notewise_connectors::MemoryStore::new();

        let refused = apply_connect(
            &db,
            &credentials,
            MicrosoftGraph::ID,
            &ConnectRequest {
                target: "client-id".into(),
                key: None,
                scopes: Vec::new(),
            },
        );
        assert!(
            matches!(refused, Err(ApiError::Conflict(ref m)) if m.contains("sign in")),
            "{refused:?}"
        );
    }

    /// And with one, it connects.
    #[test]
    fn microsoft_connects_once_a_token_is_held() {
        let db = Database::open_in_memory().expect("db");
        let credentials = notewise_connectors::MemoryStore::new();
        credentials
            .set(
                MicrosoftGraph::ID,
                REFRESH_TOKEN_KEY,
                &Secret::new("refresh"),
            )
            .expect("stores");

        apply_connect(
            &db,
            &credentials,
            MicrosoftGraph::ID,
            &ConnectRequest {
                target: "client-id".into(),
                key: None,
                scopes: vec![CALENDAR_SCOPE.into()],
            },
        )
        .expect("connects");

        // And the registry now has it as both a source and a sink, which is what makes drafts
        // reachable at all.
        let registry = build_registry(&db, &credentials).expect("builds");
        assert!(registry.source(MicrosoftGraph::ID).is_ok());
        assert!(registry.sink(MicrosoftGraph::ID).is_ok());
    }

    /// The drift this closes: `build_registry`'s comment claimed both maps and the code did one.
    #[test]
    fn a_connected_google_bridge_is_both_a_source_and_a_sink() {
        let db = Database::open_in_memory().expect("db");
        let credentials = notewise_connectors::MemoryStore::new();

        apply_connect(
            &db,
            &credentials,
            GoogleBridge::ID,
            &ConnectRequest {
                target: "https://script.google.com/macros/s/AKfy/exec".into(),
                key: Some("k".into()),
                scopes: vec![CALENDAR_SCOPE.into(), MAIL_SCOPE.into()],
            },
        )
        .expect("connects");

        let registry = build_registry(&db, &credentials).expect("builds");
        assert!(registry.source(GoogleBridge::ID).is_ok(), "calendar reads");
        assert!(registry.sink(GoogleBridge::ID).is_ok(), "draft writes");
    }

    #[test]
    fn a_connector_this_build_has_no_code_for_is_still_refused() {
        let db = Database::open_in_memory().expect("db");
        let credentials = notewise_connectors::MemoryStore::new();

        let refused = apply_connect(
            &db,
            &credentials,
            "jira",
            &ConnectRequest {
                target: "https://example.atlassian.net".into(),
                key: None,
                scopes: Vec::new(),
            },
        );
        assert!(matches!(refused, Err(ApiError::NotFound(_))), "{refused:?}");
    }

    // ------------------------------------------------------------ signing in

    /// Notewise ships no Microsoft registration, so the flow asks for one rather than failing at
    /// Microsoft with an error about an unknown application.
    #[tokio::test]
    async fn signing_in_without_a_client_id_says_what_is_needed() {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("db"),
            notewise_ai_router::Router::from_config(notewise_ai_router::RouterConfig::mock())
                .expect("mock"),
        ));

        let refused = start_microsoft_signin(axum::extract::State(state), None).await;

        match refused {
            Err(ApiError::BadRequest(message)) => {
                assert!(message.contains("app registration"), "{message}");
                assert!(message.contains("localhost"), "{message}");
            }
            other => panic!("expected a bad request, got {other:?}"),
        }
    }

    /// A sign-in reports pending immediately and does not block the request on a consent screen.
    #[tokio::test]
    async fn starting_a_sign_in_returns_a_url_and_leaves_it_pending() {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("db"),
            notewise_ai_router::Router::from_config(notewise_ai_router::RouterConfig::mock())
                .expect("mock"),
        ));

        assert_eq!(state.microsoft_signin().lock().await.state, "idle");

        let started = start_microsoft_signin(
            axum::extract::State(Arc::clone(&state)),
            Some(Json(SignInRequest {
                client_id: Some("the-tenants-client-id".into()),
                scopes: vec![CALENDAR_SCOPE.into()],
            })),
        )
        .await
        .expect("starts")
        .0;

        assert!(started.authorize_url.contains("the-tenants-client-id"));
        assert!(started.authorize_url.contains("code_challenge="));
        assert!(started.redirect_uri.starts_with("http://localhost:"));
        assert_eq!(state.microsoft_signin().lock().await.state, "pending");
    }

    /// Syncing needs a workspace on disk, and says so rather than failing obscurely.
    #[tokio::test]
    async fn syncing_an_in_memory_engine_says_why_it_cannot() {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("db"),
            notewise_ai_router::Router::from_config(notewise_ai_router::RouterConfig::mock())
                .expect("mock"),
        ));

        let refused = sync_now(axum::extract::State(state)).await;
        assert!(matches!(refused, Err(ApiError::Conflict(_))), "{refused:?}");
    }

    /// With nothing connected, a sync is a no-op rather than an error.
    #[tokio::test]
    async fn syncing_with_nothing_connected_pulls_nothing() {
        let dir = tempfile::tempdir().expect("dir");
        let state = Arc::new(AppState::new(
            Database::open(dir.path().join("notewise.db")).expect("db"),
            notewise_ai_router::Router::from_config(notewise_ai_router::RouterConfig::mock())
                .expect("mock"),
        ));

        let report = sync_now(axum::extract::State(state))
            .await
            .expect("syncs")
            .0;
        assert_eq!(report.pulled, 0);
        assert!(report.failures.is_empty());
    }

    // ------------------------------------------------------------ mail drafts

    /// The gate: an account connected for calendar only never gets a mail delivery queued against
    /// it, rather than getting one that fails at the provider with a 403.
    #[tokio::test]
    async fn a_calendar_only_account_gets_no_mail_delivery() {
        let dir = tempfile::tempdir().expect("dir");
        let state = Arc::new(AppState::new(
            Database::open(dir.path().join("notewise.db")).expect("db"),
            notewise_ai_router::Router::from_config(notewise_ai_router::RouterConfig::mock())
                .expect("mock"),
        ));

        {
            let db = state.db().await;
            ConnectorAccountRepository::new(&db)
                .connect(
                    GoogleBridge::ID,
                    Some("https://script.google.com/macros/s/AKfy/exec"),
                    &[CALENDAR_SCOPE.to_string()],
                )
                .expect("connects");
        }

        let draft = {
            let db = state.db().await;
            notewise_storage::EmailDraftRepository::new(&db)
                .create(notewise_storage::NewEmailDraft {
                    meeting_id: None,
                    subject: "Follow-up".into(),
                    body: "what we agreed".into(),
                    recipients: vec!["priya@example.com".into()],
                    variant: None,
                })
                .expect("a draft")
        };

        let refused = enqueue_mail_draft(&state, &draft).await;
        assert!(
            refused
                .as_ref()
                .err()
                .is_some_and(|e| e.contains("mail access")),
            "{refused:?}"
        );

        // And nothing was queued, which is the property that matters.
        let db = state.db().await;
        assert!(OutboxRepository::new(&db)
            .list_failed(10)
            .expect("reads")
            .is_empty());
    }

    /// Approving a draft with no vendor connected must not fail the approval.
    #[tokio::test]
    async fn approving_with_nothing_connected_still_approves() {
        let dir = tempfile::tempdir().expect("dir");
        let state = Arc::new(AppState::new(
            Database::open(dir.path().join("notewise.db")).expect("db"),
            notewise_ai_router::Router::from_config(notewise_ai_router::RouterConfig::mock())
                .expect("mock"),
        ));

        let draft = {
            let db = state.db().await;
            notewise_storage::EmailDraftRepository::new(&db)
                .create(notewise_storage::NewEmailDraft {
                    meeting_id: None,
                    subject: "Follow-up".into(),
                    body: "what we agreed".into(),
                    recipients: vec!["priya@example.com".into()],
                    variant: None,
                })
                .expect("a draft")
        };

        assert!(enqueue_mail_draft(&state, &draft).await.is_err());

        // The approval is a separate act and is unaffected.
        let db = state.db().await;
        let approved = notewise_storage::EmailDraftRepository::new(&db)
            .approve(draft.id)
            .expect("approves");
        assert_eq!(approved.status, notewise_storage::DraftStatus::Approved);
    }
}

// ---------------------------------------------------------------- signing in to Microsoft

/// Where a Microsoft sign-in has got to.
///
/// Held in memory rather than stored: the whole lifetime of one is the minute a person spends on a
/// consent screen, and a half-finished sign-in that survived a restart would be a listener on a port
/// nothing is on.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SignInStatus {
    /// `idle`, `pending`, `connected`, or `failed`.
    pub state: &'static str,
    /// Why it failed, when it did. Shown verbatim.
    pub error: Option<String>,
}

impl SignInStatus {
    pub fn idle() -> Self {
        Self {
            state: "idle",
            error: None,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct SignInRequest {
    /// A tenant's own app registration, for policies that require one.
    ///
    /// Optional in the request and required in practice: Notewise ships no registration of its own
    /// yet, so without this there is no client to authorize. Stated in the refusal rather than
    /// pretended around.
    #[serde(default)]
    pub client_id: Option<String>,
    /// `calendar`, `mail`, or both. Recorded on the account when the sign-in completes.
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SignInStarted {
    /// Where to send the user. The shell opens it; a browser does the rest.
    pub authorize_url: String,
    pub redirect_uri: String,
}

/// Begin a Microsoft sign-in.
///
/// Opens a loopback listener, returns the consent URL, and completes in the background — the request
/// does not wait, because the thing being waited on is a person reading a permissions screen.
///
/// # Why a client id is required
///
/// D8 says Notewise ships its own multi-tenant registration and the client id is embedded in the
/// binary, which is correct for a public PKCE client. No such registration exists yet, and
/// hardcoding a plausible-looking id would produce a sign-in that fails at Microsoft with an error
/// about an unknown application. So the flow is complete and asks for one, which is also the
/// bring-your-own path D8 wanted anyway.
pub async fn start_microsoft_signin(
    State(state): State<Arc<AppState>>,
    body: Option<Json<SignInRequest>>,
) -> ApiResult<Json<SignInStarted>> {
    let body = body.map(|Json(body)| body).unwrap_or_default();

    let client_id = match body.client_id.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => {
            // Reuse one already stored, so reconnecting does not mean retyping it.
            KeychainStore::new()
                .get(MicrosoftGraph::ID, notewise_connectors::CLIENT_ID_KEY)
                .map_err(|e| ApiError::Internal(format!("cannot read the client id: {e}")))?
                .map(|secret| secret.expose().to_string())
                .ok_or_else(|| {
                    ApiError::BadRequest(
                        "Notewise has no Microsoft app registration of its own yet, so this needs \
                         the client id of an app registration in your tenant. Azure Portal → App \
                         registrations → New, with a public client redirect of http://localhost."
                            .into(),
                    )
                })?
        }
    };

    let pending = notewise_connectors::PendingAuth::start(&client_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let started = SignInStarted {
        authorize_url: pending.authorize_url().to_string(),
        redirect_uri: pending.redirect_uri().to_string(),
    };

    *state.microsoft_signin().lock().await = SignInStatus {
        state: "pending",
        error: None,
    };

    let scopes = if body.scopes.is_empty() {
        vec![CALENDAR_SCOPE.to_string()]
    } else {
        body.scopes.clone()
    };

    // The background half. Everything after this point happens while the user is on Microsoft's
    // page, and its only output is the status the settings screen polls.
    let task_state = Arc::clone(&state);
    tokio::spawn(async move {
        let outcome = complete_microsoft_signin(&task_state, pending, &client_id, &scopes).await;

        *task_state.microsoft_signin().lock().await = match outcome {
            Ok(()) => SignInStatus {
                state: "connected",
                error: None,
            },
            Err(e) => SignInStatus {
                state: "failed",
                error: Some(e),
            },
        };
    });

    Ok(Json(started))
}

/// Wait for the redirect, store the token, and register the connector.
async fn complete_microsoft_signin(
    state: &Arc<AppState>,
    pending: notewise_connectors::PendingAuth,
    client_id: &str,
    scopes: &[String],
) -> Result<(), String> {
    let code = pending.wait_for_code().await.map_err(|e| e.to_string())?;

    let refresh = pending
        .exchange(
            client_id,
            &code,
            "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            &reqwest::Client::new(),
        )
        .await
        .map_err(|e| e.to_string())?;

    let credentials = KeychainStore::new();
    credentials
        .set(MicrosoftGraph::ID, REFRESH_TOKEN_KEY, &refresh)
        .map_err(|e| format!("cannot store the token: {e}"))?;
    // Stored so reconnecting does not mean retyping it.
    credentials
        .set(
            MicrosoftGraph::ID,
            notewise_connectors::CLIENT_ID_KEY,
            &Secret::new(client_id),
        )
        .map_err(|e| format!("cannot store the client id: {e}"))?;

    let db = state.db().await;
    ConnectorAccountRepository::new(&db)
        .connect(MicrosoftGraph::ID, Some(client_id), scopes)
        .map_err(|e| e.to_string())?;

    let registry =
        build_registry(&db, &credentials).map_err(|e| format!("cannot rebuild connectors: {e}"))?;
    drop(db);
    state.set_connectors(registry);

    Ok(())
}

/// How the sign-in is going.
pub async fn microsoft_signin_status(State(state): State<Arc<AppState>>) -> Json<SignInStatus> {
    Json(state.microsoft_signin().lock().await.clone())
}

// ---------------------------------------------------------------- pulling

#[derive(Debug, Serialize)]
pub struct SyncReport {
    pub pulled: usize,
    pub upserted: usize,
    /// Connectors that failed, and why. One bad account must not hide the others.
    pub failures: Vec<String>,
}

/// Pull every connected source once.
///
/// Synchronous, because the user pressed a button. There is no background pull loop yet, so this is
/// how calendar events arrive at all — worth saying plainly rather than leaving somebody to wonder
/// why nothing appeared overnight.
pub async fn sync_now(State(state): State<Arc<AppState>>) -> ApiResult<Json<SyncReport>> {
    let Some(db_path) = state.db_path().map(std::path::Path::to_path_buf) else {
        return Err(ApiError::Conflict(
            "syncing needs a workspace stored on disk; this engine is in memory only".into(),
        ));
    };

    let registry = state.connectors();

    // Its own connection on its own thread, for the reason the vault mirror needs one: `Importer`
    // borrows the database across its awaits and `Database` is `Send` but not `Sync`.
    let report = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let db = notewise_storage::Database::open(&db_path).map_err(|e| e.to_string())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;

        runtime
            .block_on(notewise_connectors::Importer::new(registry).run(&db))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| ApiError::Internal(format!("the sync thread stopped: {e}")))?
    .map_err(ApiError::Internal)?;

    Ok(Json(SyncReport {
        pulled: report.pulled,
        upserted: report.upserted,
        failures: report
            .failures
            .into_iter()
            .map(|(id, error)| format!("{id}: {error}"))
            .collect(),
    }))
}

// ---------------------------------------------------------------- mail drafts

/// Put an approved draft in the user's mailbox.
///
/// # Why the scope is checked here and not in the sink
///
/// A connector knows how to create a draft; it does not know whether the user agreed to let it. That
/// lives on `ConnectorAccount.scopes`, which is a database row, and the sink has no database. So the
/// gate is at the enqueue: an account connected for calendar only never gets a mail delivery queued
/// against it, rather than getting one that fails at the provider with a 403.
///
/// Enqueue and drain in the same call, because the alternative is a queue nothing drains — see
/// [`crate::vault`] for the same reasoning at more length.
pub async fn enqueue_mail_draft(
    state: &Arc<AppState>,
    draft: &notewise_storage::EmailDraft,
) -> Result<(), String> {
    let Some(db_path) = state.db_path().map(std::path::Path::to_path_buf) else {
        return Err("mailbox drafts need a workspace stored on disk".into());
    };

    // The first connected account that may write mail. One vendor at a time: a draft in two
    // mailboxes is two drafts to remember not to send.
    let connector_id = {
        let db = state.db().await;
        ConnectorAccountRepository::new(&db)
            .list()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|account| {
                matches!(
                    account.connector_id.as_str(),
                    GoogleBridge::ID | MicrosoftGraph::ID
                ) && account.scopes.iter().any(|scope| scope == MAIL_SCOPE)
            })
            .map(|account| account.connector_id)
    };

    let Some(connector_id) = connector_id else {
        return Err(
            "no account is connected with mail access, so the draft stays in Notewise".into(),
        );
    };

    let payload = serde_json::json!({
        "to": draft.recipients,
        "subject": draft.subject,
        "body": draft.body,
    })
    .to_string();

    // Keyed on the draft, so approving twice cannot make two drafts in the mailbox — and the sink
    // returns the existing artifact even if a row somehow reached it twice.
    let key = format!("{connector_id}:email_draft:{}", draft.id);
    let draft_id = draft.id;
    let registry = state.connectors();

    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let db = notewise_storage::Database::open(&db_path).map_err(|e| e.to_string())?;

        OutboxRepository::new(&db)
            .enqueue(notewise_storage::NewOutboxEntry {
                connector_id,
                node_kind: "email_draft".to_string(),
                node_id: draft_id,
                operation: notewise_connectors::Operation::Create.as_str().to_string(),
                payload,
                idempotency_key: key,
            })
            .map_err(|e| e.to_string())?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;

        let dispatcher = notewise_connectors::Dispatcher::new(
            registry,
            notewise_connectors::RetryPolicy::default(),
        );
        runtime
            .block_on(dispatcher.drain(&db))
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("the delivery thread stopped: {e}"))?
}
