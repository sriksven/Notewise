//! A connector that talks to nothing.
//!
//! Public on purpose, for the reason `notewise-ai-router` keeps `MockBackend` public: a
//! boundary is only protected if it is testable. Without this, every test touching delivery
//! would need a live vendor account, those tests would get skipped, and the seam would
//! quietly erode.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::connector::{Connector, SinkConnector};
use crate::error::{ConnectorError, Result};
use crate::types::{ExternalRef, Health, Outbound};

type FailureFn = Box<dyn Fn() -> ConnectorError + Send + Sync>;

pub struct MockConnector {
    id: String,
    pushed: Mutex<Vec<Outbound>>,
    failure: Option<FailureFn>,
}

impl std::fmt::Debug for MockConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockConnector")
            .field("id", &self.id)
            .field("pushed", &self.pushed.lock().map(|p| p.len()).unwrap_or(0))
            .field("failing", &self.failure.is_some())
            .finish()
    }
}

impl MockConnector {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            pushed: Mutex::new(Vec::new()),
            failure: None,
        }
    }

    /// Make every push fail, so retry and dead-letter paths can be exercised.
    pub fn failing_with(
        mut self,
        failure: impl Fn() -> ConnectorError + Send + Sync + 'static,
    ) -> Self {
        self.failure = Some(Box::new(failure));
        self
    }

    /// Everything successfully pushed, in order.
    pub fn pushed(&self) -> Vec<Outbound> {
        self.pushed.lock().expect("mock mutex poisoned").clone()
    }
}

#[async_trait]
impl Connector for MockConnector {
    fn id(&self) -> &str {
        &self.id
    }

    fn display_name(&self) -> &str {
        "Mock"
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn health(&self) -> Result<Health> {
        Ok(Health::Ok)
    }
}

#[async_trait]
impl SinkConnector for MockConnector {
    async fn push(&self, outbound: &Outbound) -> Result<ExternalRef> {
        if let Some(failure) = &self.failure {
            return Err(failure());
        }

        let mut pushed = self.pushed.lock().expect("mock mutex poisoned");
        pushed.push(outbound.clone());

        Ok(ExternalRef::new(format!("mock-{}", pushed.len())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Operation;
    use notewise_storage::Id;

    fn outbound() -> Outbound {
        Outbound {
            node_kind: "meeting".into(),
            node_id: Id::new(),
            operation: Operation::Create,
            payload: serde_json::json!({"title": "Standup"}),
            existing: None,
        }
    }

    #[tokio::test]
    async fn push_records_what_it_was_given() {
        let mock = MockConnector::new("mock");
        let sent = outbound();

        let reference = mock.push(&sent).await.unwrap();

        assert!(!reference.external_id.is_empty());
        assert_eq!(mock.pushed(), vec![sent]);
    }

    #[tokio::test]
    async fn a_failing_mock_returns_the_configured_error() {
        let mock = MockConnector::new("mock")
            .failing_with(|| ConnectorError::Transient("simulated".into()));

        let err = mock.push(&outbound()).await.unwrap_err();
        assert!(err.is_retryable());
        assert!(
            mock.pushed().is_empty(),
            "a failed push must not record a delivery"
        );
    }

    #[tokio::test]
    async fn mock_reports_itself_as_local() {
        let mock = MockConnector::new("mock");
        assert!(mock.is_local());
        assert_eq!(mock.id(), "mock");
        assert_eq!(mock.health().await.unwrap(), Health::Ok);
    }
}
