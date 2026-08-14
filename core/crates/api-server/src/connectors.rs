//! Connector status and outbox inspection.
//!
//! Read-only. Connecting an account is a separate, deliberate flow — this surface exists so a
//! user can see what is configured and what failed to deliver, because a queue whose failures
//! are invisible is worse than no queue.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use notewise_connectors::ConnectorRegistry;
use notewise_storage::{Database, OutboxRepository};
use serde::Serialize;

use crate::error::ApiResult;
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

pub async fn list_failed_deliveries(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<FailedDelivery>>> {
    // `db()` already returns the guard; there is no second `.lock()`. Held across synchronous
    // calls only — `Database` is `Send` but not `Sync`, so keeping it alive across an `.await`
    // would make this future non-`Send` and axum would reject the route.
    let db = state.db().await;
    Ok(Json(describe_failures(&db, 50)?))
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
}

#[cfg(test)]
mod route_smoke {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use notewise_ai_router::{Router, RouterConfig};
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
}
