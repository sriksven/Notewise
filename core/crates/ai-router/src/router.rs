//! Backend selection.
//!
//! [`Router`] owns a boxed [`AiBackend`] and forwards to it. Callers hold a `Router`, so
//! switching a user between local and cloud is a config change rather than a code change.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::backends::{
    AnthropicBackend, GeminiBackend, MockBackend, OllamaBackend, OpenAiCompatBackend, Preset,
};
use crate::error::{AiError, Result};
use crate::types::{
    ChatRequest, ChatResponse, ExtractedActionItem, ExtractedDecision, SummaryOutput,
    TranscriptInput,
};
use crate::AiBackend;

/// Which backend to use.
///
/// A flat enum rather than one variant carrying provider data, so it maps directly onto a
/// list of radio buttons in settings and serializes as a single stable string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Deterministic, in-process. No model required.
    Mock,
    /// Local inference via an Ollama daemon.
    Ollama,
    /// The user's own Anthropic API key.
    Anthropic,
    /// The user's own Google Gemini API key.
    Gemini,
    /// Groq's hosted inference.
    Groq,
    /// OpenRouter, which fronts many models behind one key.
    OpenRouter,
    /// LM Studio running on this machine.
    LmStudio,
    /// Unsloth running on this machine.
    Unsloth,
    /// Any other endpoint speaking the OpenAI chat-completions shape.
    /// Requires `RouterConfig::endpoint`.
    OpenAiCompatible,
}

impl BackendKind {
    pub const ALL: &'static [BackendKind] = &[
        BackendKind::Mock,
        BackendKind::Ollama,
        BackendKind::Anthropic,
        BackendKind::Gemini,
        BackendKind::Groq,
        BackendKind::OpenRouter,
        BackendKind::LmStudio,
        BackendKind::Unsloth,
        BackendKind::OpenAiCompatible,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            BackendKind::Mock => "mock",
            BackendKind::Ollama => "ollama",
            BackendKind::Anthropic => "anthropic",
            BackendKind::Gemini => "gemini",
            BackendKind::Groq => "groq",
            BackendKind::OpenRouter => "openrouter",
            BackendKind::LmStudio => "lmstudio",
            BackendKind::Unsloth => "unsloth",
            BackendKind::OpenAiCompatible => "openai_compatible",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s)
    }

    /// Human-readable name, for settings UI.
    pub fn label(&self) -> &'static str {
        match self {
            BackendKind::Mock => "Mock (no model)",
            BackendKind::Ollama => "Ollama",
            BackendKind::Anthropic => "Anthropic",
            BackendKind::Gemini => "Google Gemini",
            BackendKind::Groq => "Groq",
            BackendKind::OpenRouter => "OpenRouter",
            BackendKind::LmStudio => "LM Studio",
            BackendKind::Unsloth => "Unsloth",
            BackendKind::OpenAiCompatible => "OpenAI-compatible endpoint",
        }
    }

    /// Whether this backend keeps transcripts on the user's machine.
    ///
    /// Answerable without constructing a backend, so settings UI can show the privacy
    /// implication of each option before the user picks one.
    ///
    /// [`BackendKind::OpenAiCompatible`] reports `false` because a custom endpoint's
    /// locality depends on its URL, which this enum does not carry. Claiming "local" when
    /// unsure would be the dangerous direction to be wrong in; the constructed backend
    /// reports the true answer via [`Router::is_local`].
    pub fn is_local(&self) -> bool {
        matches!(
            self,
            BackendKind::Mock | BackendKind::Ollama | BackendKind::LmStudio | BackendKind::Unsloth
        )
    }

    pub fn requires_api_key(&self) -> bool {
        matches!(
            self,
            BackendKind::Anthropic
                | BackendKind::Gemini
                | BackendKind::Groq
                | BackendKind::OpenRouter
        )
    }

    /// Whether this backend needs an explicit endpoint URL.
    pub fn requires_endpoint(&self) -> bool {
        matches!(self, BackendKind::OpenAiCompatible)
    }

    /// Whether a user should ever be offered this backend.
    ///
    /// [`BackendKind::Mock`] is not. It exists so the seam stays testable, and it answers every
    /// request with fixed text — a user who picks it out of a menu gets summaries and answers
    /// that were never derived from their meeting, presented exactly like real ones. That is
    /// the worst failure this product can have, and it should not be one menu click away.
    ///
    /// It stays in [`BackendKind::ALL`] so `NOTEWISE_BACKEND=mock` still works for development.
    pub fn is_selectable(&self) -> bool {
        !matches!(self, BackendKind::Mock)
    }

    /// Whether the models this backend can run are discoverable by asking it.
    ///
    /// True for local daemons, which hold whatever the user has pulled and can be listed. The
    /// hosted providers have catalogues that change without us, and asking them costs a
    /// round trip against a metered key.
    pub fn lists_models(&self) -> bool {
        matches!(self, BackendKind::Ollama | BackendKind::LmStudio)
    }

    /// The OpenAI-compatible preset for this kind, if it is one.
    fn preset(&self, endpoint: Option<String>) -> Option<Preset> {
        Some(match self {
            BackendKind::Groq => Preset::Groq,
            BackendKind::OpenRouter => Preset::OpenRouter,
            BackendKind::LmStudio => Preset::LmStudio,
            BackendKind::Unsloth => Preset::Unsloth,
            BackendKind::OpenAiCompatible => Preset::Custom {
                name: "custom".to_string(),
                base_url: endpoint?,
            },
            _ => return None,
        })
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
    /// Overrides the backend's default endpoint. Required for `OpenAiCompatible`.
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
    pub fn new(backend: BackendKind) -> Self {
        Self {
            backend,
            api_key: None,
            model: None,
            endpoint: None,
        }
    }

    pub fn mock() -> Self {
        Self::new(BackendKind::Mock)
    }

    pub fn ollama() -> Self {
        Self::new(BackendKind::Ollama)
    }

    pub fn anthropic(api_key: impl Into<String>) -> Self {
        Self::new(BackendKind::Anthropic).with_api_key(api_key)
    }

    pub fn gemini(api_key: impl Into<String>) -> Self {
        Self::new(BackendKind::Gemini).with_api_key(api_key)
    }

    pub fn groq(api_key: impl Into<String>) -> Self {
        Self::new(BackendKind::Groq).with_api_key(api_key)
    }

    pub fn openrouter(api_key: impl Into<String>) -> Self {
        Self::new(BackendKind::OpenRouter).with_api_key(api_key)
    }

    pub fn lm_studio() -> Self {
        Self::new(BackendKind::LmStudio)
    }

    /// Any endpoint speaking the OpenAI chat-completions shape.
    pub fn openai_compatible(endpoint: impl Into<String>) -> Self {
        Self::new(BackendKind::OpenAiCompatible).with_endpoint(endpoint)
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
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
    kind: BackendKind,
}

impl Router {
    /// Build a router from configuration.
    pub fn from_config(config: RouterConfig) -> Result<Self> {
        let kind = config.backend;

        let backend: Box<dyn AiBackend> = match kind {
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
                let mut backend = AnthropicBackend::new(config.api_key.unwrap_or_default())?;
                if let Some(model) = config.model {
                    backend = backend.with_model(model);
                }
                if let Some(endpoint) = config.endpoint {
                    backend = backend.with_endpoint(endpoint);
                }
                Box::new(backend)
            }

            BackendKind::Gemini => {
                let mut backend = GeminiBackend::new(config.api_key.unwrap_or_default())?;
                if let Some(model) = config.model {
                    backend = backend.with_model(model);
                }
                Box::new(backend)
            }

            // Every remaining kind is the same client behind a different base URL.
            _ => {
                let preset = kind.preset(config.endpoint).ok_or_else(|| {
                    AiError::InvalidRequest(format!("{} requires an endpoint URL", kind.label()))
                })?;

                let mut backend = OpenAiCompatBackend::new(preset, config.api_key)?;
                if let Some(model) = config.model {
                    backend = backend.with_model(model);
                }
                Box::new(backend)
            }
        };

        Ok(Self { backend, kind })
    }

    /// Wrap an already-constructed backend. Mainly useful in tests.
    pub fn with_backend(backend: Box<dyn AiBackend>) -> Self {
        Self {
            backend,
            kind: BackendKind::Mock,
        }
    }

    /// Which backend kind this router was built from.
    pub fn kind(&self) -> BackendKind {
        self.kind
    }

    /// Whether the active backend keeps data on the user's machine.
    ///
    /// Asks the backend rather than the kind, so a custom endpoint pointing at localhost is
    /// correctly reported as local.
    pub fn is_local(&self) -> bool {
        self.backend.is_local()
    }

    pub fn model_id(&self) -> &str {
        self.backend.model_id()
    }

    /// Whether the active backend is usable right now. See [`AiBackend::probe`].
    pub async fn probe(&self) -> Result<()> {
        self.backend.probe().await
    }

    /// What the active backend can run. See [`AiBackend::installed_models`].
    pub async fn installed_models(&self) -> Result<Vec<String>> {
        self.backend.installed_models().await
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

    #[test]
    fn default_config_is_local() {
        assert!(
            RouterConfig::default().backend.is_local(),
            "a default install must not send meeting content off-device"
        );
    }

    #[test]
    fn privacy_is_answerable_without_constructing_a_backend() {
        assert!(BackendKind::Mock.is_local());
        assert!(BackendKind::Ollama.is_local());
        assert!(BackendKind::LmStudio.is_local());
        assert!(BackendKind::Unsloth.is_local());

        assert!(!BackendKind::Anthropic.is_local());
        assert!(!BackendKind::Gemini.is_local());
        assert!(!BackendKind::Groq.is_local());
        assert!(!BackendKind::OpenRouter.is_local());
    }

    #[test]
    fn an_unknown_custom_endpoint_is_assumed_remote() {
        // Wrong in the safe direction: claiming "local" when unsure would understate
        // where a user's transcripts go.
        assert!(!BackendKind::OpenAiCompatible.is_local());
    }

    #[test]
    fn a_custom_endpoint_on_localhost_reports_as_local_once_built() {
        let router =
            Router::from_config(RouterConfig::openai_compatible("http://localhost:8080/v1"))
                .unwrap();

        assert!(
            router.is_local(),
            "the constructed backend knows its URL and should report the truth"
        );
    }

    #[test]
    fn only_hosted_backends_need_a_key() {
        for kind in [
            BackendKind::Anthropic,
            BackendKind::Gemini,
            BackendKind::Groq,
            BackendKind::OpenRouter,
        ] {
            assert!(kind.requires_api_key(), "{kind:?}");
        }
        for kind in [
            BackendKind::Mock,
            BackendKind::Ollama,
            BackendKind::LmStudio,
            BackendKind::Unsloth,
        ] {
            assert!(!kind.requires_api_key(), "{kind:?}");
        }
    }

    #[test]
    fn hosted_backends_without_a_key_fail_at_construction() {
        for kind in [
            BackendKind::Anthropic,
            BackendKind::Gemini,
            BackendKind::Groq,
            BackendKind::OpenRouter,
        ] {
            let err = Router::from_config(RouterConfig::new(kind))
                .expect_err("{kind:?} should refuse to build without a key");
            assert!(
                matches!(err, AiError::MissingApiKey { .. }),
                "{kind:?}: {err:?}"
            );
        }
    }

    #[test]
    fn a_custom_endpoint_is_required_for_the_generic_kind() {
        let err = Router::from_config(RouterConfig::new(BackendKind::OpenAiCompatible))
            .expect_err("should refuse without a URL");

        assert!(err.to_string().contains("endpoint"), "{err}");
    }

    #[test]
    fn every_kind_round_trips_and_has_a_label() {
        let mut seen = std::collections::HashSet::new();
        for kind in BackendKind::ALL {
            assert_eq!(BackendKind::parse(kind.as_str()), Some(*kind));
            assert!(seen.insert(kind.as_str()), "duplicate string for {kind:?}");
            assert!(!kind.label().is_empty());
        }
        assert_eq!(BackendKind::parse("telepathy"), None);
    }

    #[test]
    fn local_backends_build_without_credentials() {
        for config in [
            RouterConfig::mock(),
            RouterConfig::ollama(),
            RouterConfig::lm_studio(),
            RouterConfig::new(BackendKind::Unsloth),
        ] {
            let router = Router::from_config(config.clone())
                .unwrap_or_else(|e| panic!("{:?} should build: {e}", config.backend));
            assert!(router.is_local(), "{:?}", config.backend);
        }
    }

    #[test]
    fn hosted_backends_build_with_a_key_and_report_as_remote() {
        for config in [
            RouterConfig::anthropic("sk-ant-test"),
            RouterConfig::gemini("AIza-test"),
            RouterConfig::groq("gsk-test"),
            RouterConfig::openrouter("sk-or-test"),
        ] {
            let router = Router::from_config(config.clone()).expect("should build");
            assert!(!router.is_local(), "{:?}", config.backend);
        }
    }

    #[test]
    fn the_router_remembers_which_kind_built_it() {
        let router = Router::from_config(RouterConfig::groq("gsk-test")).unwrap();
        assert_eq!(router.kind(), BackendKind::Groq);
    }

    #[test]
    fn model_override_is_applied() {
        let router =
            Router::from_config(RouterConfig::groq("gsk-test").with_model("mixtral-8x7b-32768"))
                .unwrap();
        assert!(
            router.model_id().contains("mixtral"),
            "{}",
            router.model_id()
        );
    }

    #[test]
    fn config_round_trips_through_json() {
        let config = RouterConfig::openrouter("sk-or-test").with_model("anthropic/claude-3.5");
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<RouterConfig>(&json).unwrap(), config);
    }

    #[tokio::test]
    async fn router_forwards_every_method_to_its_backend() {
        let router = Router::from_config(RouterConfig::mock()).unwrap();
        let input = TranscriptInput::new("Sync", "We agreed to ship Friday.");

        assert!(router
            .summarize(&input)
            .await
            .unwrap()
            .text
            .contains("Sync"));
        assert_eq!(router.extract_decisions(&input).await.unwrap().len(), 1);
        assert_eq!(router.extract_action_items(&input).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_router_is_itself_a_backend() {
        let inner = Router::from_config(RouterConfig::mock()).unwrap();
        let outer = Router::with_backend(Box::new(inner));

        assert!(outer
            .summarize(&TranscriptInput::new("Sync", "text"))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn backend_errors_propagate_through_the_router() {
        let router = Router::with_backend(Box::new(MockBackend::failing("simulated outage")));
        let err = router
            .summarize(&TranscriptInput::new("Sync", "text"))
            .await
            .expect_err("should fail");

        assert!(matches!(err, AiError::InvalidRequest(_)));
    }
}
