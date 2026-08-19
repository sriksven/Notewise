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
use crate::redact::{RedactionPolicy, RedactionReport};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, ExtractedActionItem, ExtractedDecision, SummaryOutput,
    TranscriptInput,
};
use crate::policy::{Predicate, RequestFacts, RouteSpec, TaskKind};
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
    /// How much to mask before text leaves the machine.
    ///
    /// Ignored for local backends — nothing leaves, so there is nothing to mask.
    pub redaction: RedactionPolicy,
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
            redaction: RedactionPolicy::Secrets,
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
            redaction: RedactionPolicy::Secrets,
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

/// The local hour, read at the request boundary so [`RequestFacts`] stays pure and testable.
fn local_hour() -> u8 {
    use chrono::Timelike;
    chrono::Local::now().hour() as u8
}

/// The masking a given destination needs.
///
/// A local backend is always `Off`: nothing leaves the machine, so masking would only degrade the
/// input for no privacy benefit.
fn policy_for(backend: &dyn AiBackend, configured: RedactionPolicy) -> RedactionPolicy {
    if backend.is_local() {
        RedactionPolicy::Off
    } else {
        configured
    }
}

/// Where a request is going, and what has to be masked before it gets there.
///
/// The redaction travels with the choice on purpose. Computing it from the router's default
/// backend would send an unmasked transcript to a remote route whenever the default is local —
/// silently, with no error, which is the worst failure this crate could have.
struct Selected<'a> {
    backend: &'a dyn AiBackend,
    name: Option<&'a str>,
    redaction: RedactionPolicy,
}

/// One configured route: its conditions, its backend, and that backend's own privacy settings.
#[derive(Debug)]
pub struct Route {
    spec: RouteSpec,
    backend: Box<dyn AiBackend>,
    kind: BackendKind,
    redaction: RedactionPolicy,
}

/// The interface every feature depends on.
#[derive(Debug)]
pub struct Router {
    backend: Box<dyn AiBackend>,
    kind: BackendKind,
    redaction: RedactionPolicy,
    /// Ordered. First match wins. Empty means every request goes to `backend`, which is exactly
    /// the behaviour before routing existed.
    routes: Vec<Route>,
}

impl Router {
    /// Build a router from configuration.
    pub fn from_config(config: RouterConfig) -> Result<Self> {
        let kind = config.backend;
        let redaction = config.redaction;

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

        Ok(Self {
            backend,
            kind,
            redaction,
            routes: Vec::new(),
        })
    }

    /// Wrap an already-constructed backend. Mainly useful in tests.
    pub fn with_backend(backend: Box<dyn AiBackend>) -> Self {
        Self {
            backend,
            kind: BackendKind::Mock,
            redaction: RedactionPolicy::Secrets,
            routes: Vec::new(),
        }
    }

    /// Override how much is masked before text leaves the machine.
    pub fn with_redaction(mut self, redaction: RedactionPolicy) -> Self {
        self.redaction = redaction;
        self
    }

    /// Add a route, evaluated after every route already added.
    pub fn with_route(
        mut self,
        spec: RouteSpec,
        backend: Box<dyn AiBackend>,
        kind: BackendKind,
        redaction: RedactionPolicy,
    ) -> Self {
        self.routes.push(Route {
            spec,
            backend,
            kind,
            redaction,
        });
        self
    }

    /// Route names, in evaluation order. For the settings UI and the explain endpoint.
    pub fn route_names(&self) -> Vec<String> {
        self.routes.iter().map(|r| r.spec.name.clone()).collect()
    }

    /// The backend a request with these facts goes to, with the masking that destination needs.
    ///
    /// Borrowed rather than cloned: this runs before every model call.
    fn route_for(&self, facts: &RequestFacts) -> Selected<'_> {
        // Iterate rather than reusing `policy::select_index`, which takes a `&[RouteSpec]` and
        // would mean cloning every spec on a path that runs before every model call. The
        // first-match rule is one line either way; the allocation is not.
        match self.routes.iter().find(|r| r.spec.matches(facts)) {
            Some(route) => Selected {
                backend: route.backend.as_ref(),
                name: Some(route.spec.name.as_str()),
                redaction: policy_for(route.backend.as_ref(), route.redaction),
            },
            None => Selected {
                backend: self.backend.as_ref(),
                name: None,
                redaction: policy_for(self.backend.as_ref(), self.redaction),
            },
        }
    }

    /// Which route a request would take. Answers "why did this cost money".
    pub fn explain(&self, facts: &RequestFacts) -> String {
        match self.route_for(facts).name {
            Some(name) => format!("route {name:?}"),
            None => "the default backend".to_string(),
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

impl Router {
    /// The policy actually in force for this router.
    ///
    /// A local backend is always `Off`: nothing leaves the machine, so masking would only
    /// degrade the model's input for no benefit. This is why redaction lives on the router
    /// rather than in each caller — the decision depends on which backend is active, which
    /// callers should not have to know.
    pub fn effective_redaction(&self) -> RedactionPolicy {
        if self.backend.is_local() {
            RedactionPolicy::Off
        } else {
            self.redaction
        }
    }

    /// Mask a transcript on its way out, logging what was masked but never what it was.
    ///
    /// Takes the policy rather than asking [`Self::effective_redaction`] for it, because with
    /// routing the answer depends on which destination this particular call selected.
    fn guard_transcript(&self, input: &TranscriptInput, policy: RedactionPolicy) -> TranscriptInput {
        if policy == RedactionPolicy::Off {
            return input.clone();
        }

        let (text, report) = crate::redact::redact(&input.text, policy);
        let (context, context_report) = match input.context.as_deref() {
            Some(c) => {
                let (masked, r) = crate::redact::redact(c, policy);
                (Some(masked.into_owned()), r)
            }
            None => (None, RedactionReport::default()),
        };

        if !report.is_empty() || !context_report.is_empty() {
            tracing::info!(
                backend = self.kind.label(),
                redacted = %report,
                context_redacted = %context_report,
                "masked secrets before sending to a non-local backend"
            );
        }

        TranscriptInput {
            title: input.title.clone(),
            text: text.into_owned(),
            context,
        }
    }

    fn guard_chat(&self, request: &ChatRequest, policy: RedactionPolicy) -> ChatRequest {
        if policy == RedactionPolicy::Off {
            return request.clone();
        }

        let mut report = RedactionReport::default();
        let context = request
            .context
            .iter()
            .map(|c| {
                let (masked, r) = crate::redact::redact(c, policy);
                report.merge(&r);
                masked.into_owned()
            })
            .collect();

        // User messages too. A user pasting a key into the chat box is at least as likely as
        // one being spoken in the meeting.
        let messages = request
            .messages
            .iter()
            .map(|m| {
                let (masked, r) = crate::redact::redact(&m.content, policy);
                report.merge(&r);
                ChatMessage {
                    role: m.role,
                    content: masked.into_owned(),
                }
            })
            .collect();

        if !report.is_empty() {
            tracing::info!(
                backend = self.kind.label(),
                redacted = %report,
                "masked secrets before sending to a non-local backend"
            );
        }

        ChatRequest { context, messages }
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
        let facts = RequestFacts::for_transcript(
            TaskKind::Summarize,
            &input.title,
            &input.text,
            input.context.as_deref(),
            local_hour(),
        );
        let selected = self.route_for(&facts);
        selected
            .backend
            .summarize(&self.guard_transcript(input, selected.redaction))
            .await
    }

    async fn extract_decisions(&self, input: &TranscriptInput) -> Result<Vec<ExtractedDecision>> {
        let facts = RequestFacts::for_transcript(
            TaskKind::ExtractDecisions,
            &input.title,
            &input.text,
            input.context.as_deref(),
            local_hour(),
        );
        let selected = self.route_for(&facts);
        selected
            .backend
            .extract_decisions(&self.guard_transcript(input, selected.redaction))
            .await
    }

    async fn extract_action_items(
        &self,
        input: &TranscriptInput,
    ) -> Result<Vec<ExtractedActionItem>> {
        let facts = RequestFacts::for_transcript(
            TaskKind::ExtractActionItems,
            &input.title,
            &input.text,
            input.context.as_deref(),
            local_hour(),
        );
        let selected = self.route_for(&facts);
        selected
            .backend
            .extract_action_items(&self.guard_transcript(input, selected.redaction))
            .await
    }

    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
        let last = request
            .messages
            .last()
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        let facts = RequestFacts::for_chat(&request.context, last, local_hour());
        let selected = self.route_for(&facts);
        selected
            .backend
            .chat(&self.guard_chat(request, selected.redaction))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records exactly what the backend was handed, so a test can assert on what would have
    /// gone over the wire rather than on what the router intended to send.
    #[derive(Debug, Default)]
    struct SpyBackend {
        local: bool,
        seen: std::sync::Mutex<Vec<String>>,
    }

    impl SpyBackend {
        fn cloud() -> Self {
            Self {
                local: false,
                seen: Default::default(),
            }
        }

        fn local() -> Self {
            Self {
                local: true,
                seen: Default::default(),
            }
        }

        fn transmitted(&self) -> String {
            self.seen.lock().expect("spy lock").join("\n")
        }
    }

    #[async_trait]
    impl AiBackend for std::sync::Arc<SpyBackend> {
        fn model_id(&self) -> &str {
            "spy"
        }

        fn is_local(&self) -> bool {
            self.local
        }

        async fn summarize(&self, input: &TranscriptInput) -> Result<SummaryOutput> {
            self.seen.lock().expect("spy lock").push(input.text.clone());
            Ok(SummaryOutput {
                text: String::new(),
                model: "spy".into(),
            })
        }

        async fn extract_decisions(
            &self,
            input: &TranscriptInput,
        ) -> Result<Vec<ExtractedDecision>> {
            self.seen.lock().expect("spy lock").push(input.text.clone());
            Ok(Vec::new())
        }

        async fn extract_action_items(
            &self,
            input: &TranscriptInput,
        ) -> Result<Vec<ExtractedActionItem>> {
            self.seen.lock().expect("spy lock").push(input.text.clone());
            Ok(Vec::new())
        }

        async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
            let mut seen = self.seen.lock().expect("spy lock");
            seen.extend(request.context.iter().cloned());
            seen.extend(request.messages.iter().map(|m| m.content.clone()));
            Ok(ChatResponse {
                text: String::new(),
                model: "spy".into(),
            })
        }
    }

    const SECRET: &str = "sk-ant-api03-AAAABBBBCCCCDDDDEEEEFFFF";

    /// The claim the whole module exists to support: a key spoken in a meeting does not
    /// reach a cloud provider.
    #[tokio::test]
    async fn a_secret_never_reaches_a_cloud_backend() {
        let spy = std::sync::Arc::new(SpyBackend::cloud());
        let router = Router::with_backend(Box::new(spy.clone()));
        let input = TranscriptInput::new("Standup", format!("Sam read out {SECRET} on the call"));

        router.summarize(&input).await.unwrap();
        router.extract_decisions(&input).await.unwrap();
        router.extract_action_items(&input).await.unwrap();

        let transmitted = spy.transmitted();
        assert!(
            !transmitted.contains(SECRET),
            "the key was sent to a cloud backend: {transmitted}"
        );
        assert!(transmitted.contains("[redacted:api_key]"), "{transmitted}");
        assert!(
            transmitted.contains("Sam read out"),
            "surrounding transcript must survive: {transmitted}"
        );
    }

    /// A local backend must get the real text. Masking there would degrade the summary for
    /// no privacy benefit, since nothing leaves the machine.
    #[tokio::test]
    async fn a_local_backend_receives_the_text_unchanged() {
        let spy = std::sync::Arc::new(SpyBackend::local());
        let router = Router::with_backend(Box::new(spy.clone()));

        router
            .summarize(&TranscriptInput::new("Standup", format!("key {SECRET}")))
            .await
            .unwrap();

        assert!(spy.transmitted().contains(SECRET), "local was redacted");
        assert_eq!(router.effective_redaction(), RedactionPolicy::Off);
    }

    /// A user pasting a key into the chat box is at least as likely as one being spoken.
    #[tokio::test]
    async fn chat_messages_and_context_are_masked_too() {
        let spy = std::sync::Arc::new(SpyBackend::cloud());
        let router = Router::with_backend(Box::new(spy.clone()));

        let request = ChatRequest::new(vec![ChatMessage {
            role: crate::types::Role::User,
            content: format!("is {SECRET} still valid?"),
        }])
        .with_context(vec![format!("earlier we used {SECRET}")]);

        router.chat(&request).await.unwrap();

        let transmitted = spy.transmitted();
        assert!(!transmitted.contains(SECRET), "{transmitted}");
        assert_eq!(
            transmitted.matches("[redacted:api_key]").count(),
            2,
            "both the message and the context should be masked: {transmitted}"
        );
    }

    /// Turning redaction off is possible but must be deliberate.
    #[tokio::test]
    async fn redaction_can_be_disabled_explicitly() {
        let spy = std::sync::Arc::new(SpyBackend::cloud());
        let router =
            Router::with_backend(Box::new(spy.clone())).with_redaction(RedactionPolicy::Off);

        router
            .summarize(&TranscriptInput::new("Standup", format!("key {SECRET}")))
            .await
            .unwrap();

        assert!(spy.transmitted().contains(SECRET));
    }

    #[test]
    fn redaction_defaults_to_masking_secrets() {
        assert_eq!(
            RouterConfig::default().redaction,
            RedactionPolicy::Secrets,
            "a config that forgets to set a policy must still mask"
        );
        assert_eq!(
            RouterConfig::anthropic("k").redaction,
            RedactionPolicy::Secrets
        );
    }

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

    /// A backend that reports a distinct model id, so a test can prove which one answered.
    fn named(id: &'static str) -> Box<dyn AiBackend> {
        Box::new(MockBackend::new().with_model_id(id))
    }

    #[tokio::test]
    async fn a_summary_takes_the_summary_route_and_chat_does_not() {
        let router = Router::with_backend(named("default")).with_route(
            RouteSpec {
                name: "quality".into(),
                when: vec![Predicate::Task(vec![TaskKind::Summarize])],
            },
            named("quality"),
            BackendKind::Mock,
            RedactionPolicy::Off,
        );

        let summary = router
            .summarize(&TranscriptInput::new("t", "we agreed"))
            .await
            .expect("summarizes");
        assert_eq!(summary.model, "quality");

        let answer = router
            .chat(&ChatRequest::new(vec![ChatMessage::user("hi")]))
            .await
            .expect("chats");
        assert_eq!(
            answer.model, "default",
            "chat does not match the summary route and must fall through"
        );
    }

    /// The privacy-critical case. A local default with a remote route must mask for the *route*
    /// that is actually being used — computing redaction from the default backend would send an
    /// unmasked transcript to the remote one, silently and with no error.
    #[tokio::test]
    async fn a_remote_route_masks_even_when_the_default_is_local() {
        let cloud = std::sync::Arc::new(SpyBackend::cloud());
        let router = Router::with_backend(Box::new(std::sync::Arc::new(SpyBackend::local())))
            .with_redaction(RedactionPolicy::Off)
            .with_route(
                RouteSpec {
                    name: "cloud".into(),
                    when: vec![Predicate::Task(vec![TaskKind::Summarize])],
                },
                Box::new(cloud.clone()),
                BackendKind::Anthropic,
                RedactionPolicy::Secrets,
            );

        router
            .summarize(&TranscriptInput::new(
                "Standup",
                format!("Sam read out {SECRET} on the call"),
            ))
            .await
            .expect("summarizes");

        let transmitted = cloud.transmitted();
        assert!(
            !transmitted.contains(SECRET),
            "an unmasked key reached a remote route: {transmitted}"
        );
        assert!(
            transmitted.contains("[redacted:api_key]"),
            "{transmitted}"
        );
    }

    #[test]
    fn a_router_has_no_routes_until_given_some() {
        let router = Router::from_config(RouterConfig::mock()).expect("mock router");
        assert!(
            router.route_names().is_empty(),
            "an empty policy is today's behaviour and must be the default"
        );
    }

    #[test]
    fn routes_are_named_in_the_order_they_were_added() {
        let router = Router::from_config(RouterConfig::mock())
            .expect("mock router")
            .with_route(
                RouteSpec {
                    name: "first".into(),
                    when: vec![],
                },
                Box::new(MockBackend::new()),
                BackendKind::Mock,
                RedactionPolicy::Off,
            );

        assert_eq!(router.route_names(), vec!["first".to_string()]);
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
