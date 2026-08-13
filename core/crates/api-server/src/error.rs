//! Error translation at the transport boundary.
//!
//! Core crates return their own error enums with no HTTP concerns. This module is where those
//! become status codes — keeping the mapping in one place rather than scattered through
//! handlers, and keeping transport knowledge out of `storage`, `graph`, and `ai-router`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error(transparent)]
    Storage(#[from] notewise_storage::StorageError),

    #[error(transparent)]
    Graph(#[from] notewise_graph::GraphError),

    #[error(transparent)]
    Ai(#[from] notewise_ai_router::AiError),

    #[error("{0}")]
    BadRequest(String),
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
    /// Stable machine-readable code, so clients branch on this rather than on the message.
    code: &'static str,
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str) {
        use notewise_ai_router::AiError;
        use notewise_storage::StorageError;

        match self {
            ApiError::Storage(StorageError::NotFound { .. }) => {
                (StatusCode::NOT_FOUND, "not_found")
            }
            ApiError::Storage(StorageError::Corrupt { .. }) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "invalid_state")
            }
            ApiError::Storage(StorageError::SchemaTooNew { .. }) => {
                (StatusCode::SERVICE_UNAVAILABLE, "schema_too_new")
            }
            ApiError::Storage(_) => (StatusCode::INTERNAL_SERVER_ERROR, "storage_error"),

            ApiError::Graph(notewise_graph::GraphError::DepthTooLarge(_)) => {
                (StatusCode::BAD_REQUEST, "depth_too_large")
            }
            ApiError::Graph(_) => (StatusCode::INTERNAL_SERVER_ERROR, "graph_error"),

            // A model declining is a normal outcome, not a server fault. 422 tells the
            // client the request was understood and deliberately not answered.
            ApiError::Ai(AiError::Refused { .. }) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "model_refused")
            }
            ApiError::Ai(AiError::RateLimited { .. }) => {
                (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
            }
            ApiError::Ai(AiError::MissingApiKey { .. }) => {
                (StatusCode::FAILED_DEPENDENCY, "ai_not_configured")
            }
            ApiError::Ai(AiError::InvalidRequest(_)) => (StatusCode::BAD_REQUEST, "bad_request"),
            // The local engine is fine; the upstream model is not.
            ApiError::Ai(_) => (StatusCode::BAD_GATEWAY, "ai_backend_error"),

            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = self.parts();

        if status.is_server_error() {
            tracing::error!(error = %self, %code, "request failed");
        }

        (
            status,
            Json(ErrorBody {
                error: self.to_string(),
                code,
            }),
        )
            .into_response()
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;
    use notewise_ai_router::AiError;
    use notewise_storage::{Id, StorageError};

    #[test]
    fn missing_records_map_to_404() {
        let err = ApiError::Storage(StorageError::not_found("Meeting", Id::new()));
        assert_eq!(err.parts(), (StatusCode::NOT_FOUND, "not_found"));
    }

    #[test]
    fn a_model_refusal_is_not_a_server_error() {
        let err = ApiError::Ai(AiError::Refused {
            backend: "anthropic",
            category: Some("cyber".into()),
        });
        let (status, code) = err.parts();

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(code, "model_refused");
        assert!(
            !status.is_server_error(),
            "the engine did its job; the model declined"
        );
    }

    #[test]
    fn upstream_model_failures_are_bad_gateway_not_internal_error() {
        let err = ApiError::Ai(AiError::Provider {
            backend: "anthropic",
            status: 500,
            message: "boom".into(),
        });
        assert_eq!(err.parts().0, StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn rate_limits_pass_through_as_429() {
        let err = ApiError::Ai(AiError::RateLimited {
            backend: "anthropic",
            retry_after_secs: 30,
        });
        assert_eq!(err.parts(), (StatusCode::TOO_MANY_REQUESTS, "rate_limited"));
    }

    #[test]
    fn an_unconfigured_backend_is_distinguishable_from_a_bad_request() {
        let err = ApiError::Ai(AiError::MissingApiKey {
            backend: "anthropic",
        });
        assert_eq!(err.parts(), (StatusCode::FAILED_DEPENDENCY, "ai_not_configured"));
    }

    #[test]
    fn excessive_traversal_depth_is_the_callers_fault() {
        let err = ApiError::Graph(notewise_graph::GraphError::DepthTooLarge(99));
        assert_eq!(err.parts(), (StatusCode::BAD_REQUEST, "depth_too_large"));
    }

    #[test]
    fn corrupt_stored_data_is_not_a_404() {
        let err = ApiError::Storage(StorageError::Corrupt {
            column: "meetings.source",
            reason: "unrecognized value".into(),
        });
        assert_eq!(err.parts().0, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn every_variant_carries_a_stable_code() {
        let errors = [
            ApiError::Storage(StorageError::not_found("Meeting", Id::new())),
            ApiError::Graph(notewise_graph::GraphError::DepthTooLarge(9)),
            ApiError::Ai(AiError::InvalidRequest("bad".into())),
            ApiError::BadRequest("bad".into()),
        ];

        for err in errors {
            assert!(!err.parts().1.is_empty());
        }
    }
}
