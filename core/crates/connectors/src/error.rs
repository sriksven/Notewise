use std::time::Duration;

use notewise_graph::GraphError;
use notewise_storage::StorageError;
use thiserror::Error;

/// Errors from a connector.
///
/// Variants exist to drive retry policy, not merely to describe what went wrong. The
/// dispatcher branches on these, so a new variant is a scheduling decision.
#[derive(Debug, Error)]
pub enum ConnectorError {
    #[error("{connector} rejected our credentials; reconnect the account")]
    Auth { connector: String },

    #[error("rate limited; retry after {retry_after:?}")]
    RateLimited { retry_after: Duration },

    #[error("temporary failure: {0}")]
    Transient(String),

    #[error("permanent failure: {0}")]
    Permanent(String),

    /// The destination has been changed by somebody else, and writing would destroy their work.
    ///
    /// Typed rather than a formatted `Permanent`, because something has to *act* on it: the
    /// dispatcher records the divergence so the user can be asked what to do. A message in a
    /// dead-letter row is not something a screen can offer three choices about.
    ///
    /// Not retryable. Retrying cannot resolve a conflict — only a person can.
    #[error("{path} was changed outside Notewise; not overwriting it")]
    Diverged { path: String },

    #[error("connector is not configured: {0}")]
    NotConfigured(String),

    #[error("no connector registered with id '{0}'")]
    UnknownConnector(String),

    #[error("credential store error: {0}")]
    Credential(String),

    #[error(transparent)]
    Storage(#[from] StorageError),

    #[error(transparent)]
    Graph(#[from] GraphError),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl ConnectorError {
    /// Whether the dispatcher should schedule another attempt.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ConnectorError::Transient(_) | ConnectorError::RateLimited { .. }
        )
    }

    /// A delay the remote service asked us to honour, if it named one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            ConnectorError::RateLimited { retry_after } => Some(*retry_after),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, ConnectorError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn auth_failures_are_not_retryable() {
        let err = ConnectorError::Auth {
            connector: "google_calendar".into(),
        };
        assert!(
            !err.is_retryable(),
            "retrying a rejected credential burns quota forever"
        );
    }

    #[test]
    fn transient_and_rate_limit_failures_are_retryable() {
        assert!(ConnectorError::Transient("503".into()).is_retryable());
        assert!(ConnectorError::RateLimited {
            retry_after: Duration::from_secs(30)
        }
        .is_retryable());
    }

    #[test]
    fn permanent_failures_are_not_retryable() {
        assert!(!ConnectorError::Permanent("422 malformed".into()).is_retryable());
    }

    #[test]
    fn rate_limits_report_the_vendors_own_delay() {
        let err = ConnectorError::RateLimited {
            retry_after: Duration::from_secs(90),
        };
        assert_eq!(err.retry_after(), Some(Duration::from_secs(90)));
        assert_eq!(ConnectorError::Transient("503".into()).retry_after(), None);
    }
}
