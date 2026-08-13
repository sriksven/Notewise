//! Backend selection.
//!
//! [`Router`] owns a boxed [`AiBackend`] and forwards to it. Callers hold a `Router`, so
//! switching a user between local and cloud is a config change rather than a code change.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::backends::{AnthropicBackend, MockBackend, OllamaBackend};
use crate::error::Result;
use crate::types::{
    ChatRequest, ChatResponse, ExtractedActionItem, ExtractedDecision, SummaryOutput,
    TranscriptInput,
};
use crate::AiBackend;

/// Which backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Deterministic, in-process. No model required.
    Mock,
    /// Local inference via an Ollama daemon.
    Ollama,
    /// The user's own Anthropic API key.
    Anthropic,
}

impl BackendKind {
    /// Whether this backend keeps transcripts on the user's machine.
    ///
    /// Answerable without constructing a backend, so settings UI can show the privacy
    /// implication of each option before the user picks one.
    pub fn is_local(&self) -> bool {
        matches!(self, BackendKind::Mock | BackendKind::Ollama)
    }

    pub fn requires_api_key(&self) -> bool {
        matches!(self, BackendKind::Anthropic)
    }
}

/// How to build a backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterConfig {
    pub backend: BackendKind,
    /// Required when `backend.requires_api_key()`.
    pub api_key: Option<String>,
    /// Overrides the backend's default model.
    pub model: Option<String>,
    /// Overrides the backend's default endpoint. Useful for a proxy or a non-default
    /// Ollama host.
    pub endpoint: Option<String>,
}

impl Default for RouterConfig {
    /// Defaults to local Ollama.
    ///
    /// A default install must not send meeting content anywhere, so the default backend is
    /// a local one.
    fn default() -> Self {
        Self {
            backend: BackendKind::Ollama,
            api_key: None,
            model: None,
            endpoint: None,
        }
    }
}

impl RouterConfig {
    pub fn mock() -> Self {
        Self {
            backend: BackendKind::Mock,
            ..Default::default()
        }
    }

    pub fn ollama() -> Self {
        Self::default()
    }

    pub fn anthropic(api_key: impl Into<String>) -> Self {
        Self {
            backend: BackendKind::Anthropic,
            api_key: Some(api_key.into()),
            model: None,
            endpoint: None,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }
}

/// The interface every feature depends on.
#[derive(Debug)]
pub struct Router {
    backend: Box<dyn AiBackend>,
}

impl Router {
    /// Build a router from configuration.
    pub fn from_config(config: RouterConfig) -> Result<Self> {
        let backend: Box<dyn AiBackend> = match config.backend {
            BackendKind::Mock => Box::new(MockBackend::new()),

            BackendKind::Ollama => {
                let mut backend = OllamaBackend::new();
                if let Some(model) = config.model {
                    backend = backend.with_model(model);
                }
                if let Some(endpoint) = config.endpoint {
                    backend = backend.with_endpoint(endpoint);
                }
                Box::new(backend)
            }

            BackendKind::Anthropic => {
                let key = config.api_key.unwrap_or_default();
                let mut backend = AnthropicBackend::new(key)?;
                if let Some(model) = config.model {
                    backend = backend.with_model(model);
                }
                if let Some(endpoint) = config.endpoint {
                    backend = backend.with_endpoint(endpoint);
                }
                Box::new(backend)
            }
        };

        Ok(Self { backend })
    }

    /// Wrap an already-constructed backend. Mainly useful in tests.
    pub fn with_backend(backend: Box<dyn AiBackend>) -> Self {
        Self { backend }
    }

    /// Whether the active backend keeps data on the user's machine.
    pub fn is_local(&self) -> bool {
        self.backend.is_local()
    }

    pub fn model_id(&self) -> &str {
        self.backend.model_id()
    }
}

#[async_trait]
impl AiBackend for Router {
    fn model_id(&self) -> &str {
        self.backend.model_id()
    }

    fn is_local(&self) -> bool {
        self.backend.is_local()
    }

    async fn summarize(&self, input: &TranscriptInput) -> Result<SummaryOutput> {
        self.backend.summarize(input).await
    }

    async fn extract_decisions(&self, input: &TranscriptInput) -> Result<Vec<ExtractedDecision>> {
        self.backend.extract_decisions(input).await
    }

    async fn extract_action_items(
        &self,
        input: &TranscriptInput,
    ) -> Result<Vec<ExtractedActionItem>> {
        self.backend.extract_action_items(input).await
    }

    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
        self.backend.chat(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AiError;

    #[test]
    fn default_config_is_local() {
        let config = RouterConfig::default();
        assert!(
            config.backend.is_local(),
            "a default install must not send meeting content off-device"
        );
    }

    #[test]
    fn privacy_is_answerable_without_constructing_a_backend() {
        assert!(BackendKind::Mock.is_local());
        assert!(BackendKind::Ollama.is_local());
        assert!(!BackendKind::Anthropic.is_local());
    }

    #[test]
    fn only_hosted_backends_need_a_key() {
        assert!(BackendKind::Anthropic.requires_api_key());
        assert!(!BackendKind::Ollama.requires_api_key());
        assert!(!BackendKind::Mock.requires_api_key());
    }

    #[test]
    fn anthropic_without_a_key_fails_at_construction() {
        let config = RouterConfig {
            backend: BackendKind::Anthropic,
            api_key: None,
            model: None,
            endpoint: None,
        };

        let err = Router::from_config(config).expect_err("should refuse to build");
        assert!(matches!(err, AiError::MissingApiKey { .. }));
    }

    #[test]
    fn config_selects_the_backend() {
        let mock = Router::from_config(RouterConfig::mock()).unwrap();
        assert_eq!(mock.model_id(), "mock");
        assert!(mock.is_local());

        let byok = Router::from_config(RouterConfig::anthropic("sk-ant-test")).unwrap();
        assert!(!byok.is_local());
    }

    #[test]
    fn model_override_is_applied() {
        let router = Router::from_config(RouterConfig::ollama().with_model("mistral")).unwrap();
        assert_eq!(router.model_id(), "mistral");
    }

    #[test]
    fn config_round_trips_through_json() {
        let config = RouterConfig::anthropic("sk-ant-test").with_model("claude-sonnet-5");
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<RouterConfig>(&json).unwrap(), config);
    }

    #[tokio::test]
    async fn router_forwards_every_method_to_its_backend() {
        let router = Router::from_config(RouterConfig::mock()).unwrap();
        let input = TranscriptInput::new("Sync", "We agreed to ship Friday.");

        assert!(router.summarize(&input).await.unwrap().text.contains("Sync"));
        assert_eq!(router.extract_decisions(&input).await.unwrap().len(), 1);
        assert_eq!(router.extract_action_items(&input).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_router_is_itself_a_backend() {
        // Lets a router be composed or substituted anywhere a backend is expected.
        let inner = Router::from_config(RouterConfig::mock()).unwrap();
        let outer = Router::with_backend(Box::new(inner));

        let input = TranscriptInput::new("Sync", "text");
        assert!(outer.summarize(&input).await.is_ok());
    }

    #[tokio::test]
    async fn backend_errors_propagate_through_the_router() {
        let router = Router::with_backend(Box::new(MockBackend::failing("simulated outage")));
        let input = TranscriptInput::new("Sync", "text");

        let err = router.summarize(&input).await.expect_err("should fail");
        assert!(matches!(err, AiError::InvalidRequest(_)));
    }
}
