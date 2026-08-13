//! Any provider speaking the OpenAI chat-completions shape.
//!
//! One backend rather than five. Groq, OpenRouter, LM Studio, Unsloth, together.ai, vLLM,
//! and Ollama's compatibility endpoint all accept `POST /chat/completions` with the same
//! body, so implementing them separately would be five copies of one client differing only
//! in a base URL and a header.
//!
//! It also means a provider nobody has heard of yet works via [`Preset::Custom`] without a
//! code change — which matters more than the named presets, since this is the part of the
//! ecosystem that changes fastest.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{AiError, Result};
use crate::types::{
    ChatRequest, ChatResponse, ExtractedActionItem, ExtractedDecision, Role, SummaryOutput,
    TranscriptInput,
};
use crate::AiBackend;

const BACKEND: &str = "openai-compatible";

/// A known provider, or a custom endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preset {
    /// Fast hosted inference.
    Groq,
    /// Routes to many models behind one key.
    OpenRouter,
    /// Local desktop server. Runs on the user's machine.
    LmStudio,
    /// Local server.
    Unsloth,
    /// Ollama's OpenAI-compatible endpoint, as distinct from its native API.
    OllamaCompat,
    /// Anything else that speaks the same shape.
    Custom { name: String, base_url: String },
}

impl Preset {
    /// Base URL, without the trailing `/chat/completions`.
    pub fn base_url(&self) -> &str {
        match self {
            Preset::Groq => "https://api.groq.com/openai/v1",
            Preset::OpenRouter => "https://openrouter.ai/api/v1",
            Preset::LmStudio => "http://localhost:1234/v1",
            Preset::Unsloth => "http://localhost:2024/v1",
            Preset::OllamaCompat => "http://localhost:11434/v1",
            Preset::Custom { base_url, .. } => base_url,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Preset::Groq => "groq",
            Preset::OpenRouter => "openrouter",
            Preset::LmStudio => "lmstudio",
            Preset::Unsloth => "unsloth",
            Preset::OllamaCompat => "ollama-compat",
            Preset::Custom { name, .. } => name,
        }
    }

    /// A sensible default model for this provider.
    pub fn default_model(&self) -> &str {
        match self {
            Preset::Groq => "llama-3.3-70b-versatile",
            Preset::OpenRouter => "meta-llama/llama-3.3-70b-instruct",
            Preset::LmStudio | Preset::Unsloth => "local-model",
            Preset::OllamaCompat => "llama3.1",
            Preset::Custom { .. } => "default",
        }
    }

    /// Whether this provider runs on the user's own machine.
    ///
    /// Drives the local-or-cloud badge in the UI. A locally-hosted OpenAI-compatible
    /// server is every bit as private as Ollama, and the interface should say so.
    pub fn is_local(&self) -> bool {
        match self {
            Preset::LmStudio | Preset::Unsloth | Preset::OllamaCompat => true,
            Preset::Groq | Preset::OpenRouter => false,
            // A custom endpoint is local only if it points at this machine.
            Preset::Custom { base_url, .. } => {
                let url = base_url.to_ascii_lowercase();
                url.contains("://localhost")
                    || url.contains("://127.0.0.1")
                    || url.contains("://[::1]")
                    || url.contains("://0.0.0.0")
            }
        }
    }

    /// Whether this provider requires an API key.
    pub fn requires_api_key(&self) -> bool {
        !self.is_local()
    }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "groq" => Preset::Groq,
            "openrouter" => Preset::OpenRouter,
            "lmstudio" => Preset::LmStudio,
            "unsloth" => Preset::Unsloth,
            "ollama-compat" => Preset::OllamaCompat,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatBackend {
    preset: Preset,
    api_key: Option<String>,
    model: String,
    http: reqwest::Client,
}

impl OpenAiCompatBackend {
    pub fn new(preset: Preset, api_key: Option<String>) -> Result<Self> {
        let key = api_key.filter(|k| !k.trim().is_empty());

        if preset.requires_api_key() && key.is_none() {
            return Err(AiError::MissingApiKey { backend: BACKEND });
        }

        Ok(Self {
            model: preset.default_model().to_string(),
            preset,
            api_key: key,
            http: reqwest::Client::new(),
        })
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/chat/completions",
            self.preset.base_url().trim_end_matches('/')
        )
    }

    fn body(&self, system: &str, messages: Vec<Value>, json_mode: bool) -> Value {
        let mut all = vec![json!({ "role": "system", "content": system })];
        all.extend(messages);

        let mut body = json!({
            "model": self.model,
            "messages": all,
            "stream": false,
        });

        if json_mode {
            // Widely supported across compatible providers. Some ignore it, which is why
            // the response is still parsed defensively rather than trusted.
            body["response_format"] = json!({ "type": "json_object" });
        }

        body
    }

    async fn send(&self, body: Value) -> Result<String> {
        let mut request = self.http.post(self.endpoint()).json(&body);

        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }

        let response = request.send().await.map_err(|source| AiError::Transport {
            backend: BACKEND,
            source,
        })?;

        let status = response.status();

        if status.as_u16() == 429 {
            let retry_after_secs = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(30);
            return Err(AiError::RateLimited {
                backend: BACKEND,
                retry_after_secs,
            });
        }

        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable error body>".into());
            return Err(AiError::Provider {
                backend: BACKEND,
                status: status.as_u16(),
                message,
            });
        }

        let parsed: Completion = response.json().await.map_err(|source| AiError::Transport {
            backend: BACKEND,
            source,
        })?;

        let choice =
            parsed
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| AiError::MalformedResponse {
                    backend: BACKEND,
                    reason: "response contained no choices".into(),
                })?;

        // Content filters across these providers surface as a finish_reason rather than an
        // HTTP error, so a declined request would otherwise look like an empty answer.
        if choice.finish_reason.as_deref() == Some("content_filter") {
            return Err(AiError::Refused {
                backend: BACKEND,
                category: Some("content_filter".into()),
            });
        }

        choice
            .message
            .content
            .filter(|c| !c.trim().is_empty())
            .ok_or_else(|| AiError::MalformedResponse {
                backend: BACKEND,
                reason: "response message had no content".into(),
            })
    }

    /// Parse a JSON reply, tolerating a model that wrapped it in a fenced code block.
    ///
    /// Providers that ignore `response_format` commonly do this, and failing on it would
    /// make those providers unusable for extraction when the content is actually fine.
    fn parse_json<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T> {
        let cleaned = raw.trim();
        let cleaned = cleaned
            .strip_prefix("```json")
            .or_else(|| cleaned.strip_prefix("```"))
            .map(|rest| rest.trim_start())
            .unwrap_or(cleaned);
        let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();

        serde_json::from_str(cleaned).map_err(|e| AiError::MalformedResponse {
            backend: BACKEND,
            reason: format!("expected JSON, got: {e}"),
        })
    }
}

fn transcript_prompt(input: &TranscriptInput) -> String {
    let mut prompt = format!("Meeting title: {}\n", input.title);
    if let Some(context) = &input.context {
        prompt.push_str(&format!("Context: {context}\n"));
    }
    prompt.push_str("\nTranscript:\n");
    prompt.push_str(&input.text);
    prompt
}

#[async_trait]
impl AiBackend for OpenAiCompatBackend {
    fn model_id(&self) -> &str {
        &self.model
    }

    fn is_local(&self) -> bool {
        self.preset.is_local()
    }

    async fn summarize(&self, input: &TranscriptInput) -> Result<SummaryOutput> {
        let text = self
            .send(self.body(
                "Summarize this meeting transcript. Lead with the outcome. Cover what was \
                 decided and what happens next. Omit small talk.",
                vec![json!({ "role": "user", "content": transcript_prompt(input) })],
                false,
            ))
            .await?;

        Ok(SummaryOutput {
            text,
            model: format!("{}/{}", self.preset.name(), self.model),
        })
    }

    async fn extract_decisions(&self, input: &TranscriptInput) -> Result<Vec<ExtractedDecision>> {
        let raw = self
            .send(self.body(
                "Extract the decisions reached in this meeting — things settled, not tasks. \
                 Reply with JSON only: {\"decisions\": [{\"text\": \"...\", \
                 \"reasoning\": \"...\" or null}]}. Return an empty list if nothing was decided.",
                vec![json!({ "role": "user", "content": transcript_prompt(input) })],
                true,
            ))
            .await?;

        Ok(Self::parse_json::<DecisionsWrapper>(&raw)?.decisions)
    }

    async fn extract_action_items(
        &self,
        input: &TranscriptInput,
    ) -> Result<Vec<ExtractedActionItem>> {
        let raw = self
            .send(self.body(
                "Extract the action items — work someone committed to doing. Reply with JSON \
                 only: {\"action_items\": [{\"text\": \"...\", \"owner\": \"...\" or null, \
                 \"due_hint\": \"...\" or null}]}. Keep due dates exactly as stated. Return an \
                 empty list if there are none.",
                vec![json!({ "role": "user", "content": transcript_prompt(input) })],
                true,
            ))
            .await?;

        Ok(Self::parse_json::<ActionItemsWrapper>(&raw)?.action_items)
    }

    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
        if !request.is_valid() {
            return Err(AiError::InvalidRequest(
                "chat requires a non-empty history ending with a user message".into(),
            ));
        }

        let mut system = String::from(
            "Answer questions about the user's meetings using only the material provided. \
             If the material does not contain the answer, say so plainly.",
        );
        if !request.context.is_empty() {
            system.push_str("\n\nMaterial:\n");
            system.push_str(&request.context.join("\n---\n"));
        }

        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|m| {
                json!({
                    "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                    "content": m.content,
                })
            })
            .collect();

        let text = self.send(self.body(&system, messages, false)).await?;

        Ok(ChatResponse {
            text,
            model: format!("{}/{}", self.preset.name(), self.model),
        })
    }
}

#[derive(Debug, Deserialize)]
struct DecisionsWrapper {
    decisions: Vec<ExtractedDecision>,
}

#[derive(Debug, Deserialize)]
struct ActionItemsWrapper {
    action_items: Vec<ExtractedActionItem>,
}

#[derive(Debug, Deserialize)]
struct Completion {
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChoiceMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groq() -> OpenAiCompatBackend {
        OpenAiCompatBackend::new(Preset::Groq, Some("gsk-test".into())).expect("valid")
    }

    #[test]
    fn hosted_providers_require_a_key() {
        assert!(matches!(
            OpenAiCompatBackend::new(Preset::Groq, None).unwrap_err(),
            AiError::MissingApiKey { .. }
        ));
        assert!(matches!(
            OpenAiCompatBackend::new(Preset::OpenRouter, Some("  ".into())).unwrap_err(),
            AiError::MissingApiKey { .. }
        ));
    }

    #[test]
    fn local_providers_need_no_key() {
        for preset in [Preset::LmStudio, Preset::Unsloth, Preset::OllamaCompat] {
            assert!(
                OpenAiCompatBackend::new(preset.clone(), None).is_ok(),
                "{preset:?} should not need a key"
            );
        }
    }

    #[test]
    fn locality_is_reported_correctly_per_provider() {
        // This drives the UI badge telling the user where their transcripts go.
        assert!(Preset::LmStudio.is_local());
        assert!(Preset::Unsloth.is_local());
        assert!(Preset::OllamaCompat.is_local());
        assert!(!Preset::Groq.is_local());
        assert!(!Preset::OpenRouter.is_local());
    }

    #[test]
    fn a_custom_endpoint_pointing_at_this_machine_counts_as_local() {
        for url in [
            "http://localhost:8080/v1",
            "http://127.0.0.1:5000/v1",
            "http://[::1]:8000/v1",
            "HTTP://LOCALHOST:9000/v1",
        ] {
            let preset = Preset::Custom {
                name: "self-hosted".into(),
                base_url: url.into(),
            };
            assert!(preset.is_local(), "{url} should be local");
            assert!(!preset.requires_api_key());
        }
    }

    #[test]
    fn a_custom_remote_endpoint_is_not_local() {
        let preset = Preset::Custom {
            name: "vendor".into(),
            base_url: "https://api.vendor.example/v1".into(),
        };
        assert!(!preset.is_local());
        assert!(preset.requires_api_key());
    }

    #[test]
    fn endpoints_are_built_without_double_slashes() {
        assert_eq!(
            groq().endpoint(),
            "https://api.groq.com/openai/v1/chat/completions"
        );

        let trailing = OpenAiCompatBackend::new(
            Preset::Custom {
                name: "x".into(),
                base_url: "http://localhost:9000/v1/".into(),
            },
            None,
        )
        .unwrap();
        assert_eq!(
            trailing.endpoint(),
            "http://localhost:9000/v1/chat/completions"
        );
    }

    #[test]
    fn the_system_prompt_is_the_first_message() {
        let body = groq().body(
            "be brief",
            vec![json!({"role": "user", "content": "hi"})],
            false,
        );
        let messages = body["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "be brief");
        assert_eq!(messages[1]["role"], "user");
    }

    #[test]
    fn json_mode_is_opt_in() {
        let backend = groq();
        assert!(backend
            .body("s", vec![], false)
            .get("response_format")
            .is_none());
        assert_eq!(
            backend.body("s", vec![], true)["response_format"]["type"],
            "json_object"
        );
    }

    #[test]
    fn streaming_is_disabled() {
        assert_eq!(groq().body("s", vec![], false)["stream"], false);
    }

    #[test]
    fn presets_carry_distinct_defaults() {
        let mut seen = std::collections::HashSet::new();
        for preset in [
            Preset::Groq,
            Preset::OpenRouter,
            Preset::LmStudio,
            Preset::Unsloth,
            Preset::OllamaCompat,
        ] {
            assert!(seen.insert(preset.name().to_string()));
            assert!(preset.base_url().starts_with("http"));
            assert!(!preset.default_model().is_empty());
        }
    }

    #[test]
    fn preset_names_round_trip() {
        for preset in [
            Preset::Groq,
            Preset::OpenRouter,
            Preset::LmStudio,
            Preset::Unsloth,
            Preset::OllamaCompat,
        ] {
            assert_eq!(Preset::parse(preset.name()), Some(preset));
        }
        assert_eq!(Preset::parse("nonsense"), None);
    }

    #[test]
    fn model_id_can_be_overridden() {
        let backend = groq().with_model("mixtral-8x7b-32768");
        assert_eq!(backend.model_id(), "mixtral-8x7b-32768");
    }

    #[test]
    fn plain_json_parses() {
        let parsed: DecisionsWrapper =
            OpenAiCompatBackend::parse_json(r#"{"decisions":[{"text":"Ship","reasoning":null}]}"#)
                .unwrap();
        assert_eq!(parsed.decisions.len(), 1);
    }

    #[test]
    fn fenced_json_parses() {
        // Providers that ignore response_format commonly wrap output in a code fence.
        // Failing here would make those providers unusable for extraction.
        let fenced = "```json\n{\"decisions\":[{\"text\":\"Ship\",\"reasoning\":null}]}\n```";
        let parsed: DecisionsWrapper = OpenAiCompatBackend::parse_json(fenced).unwrap();
        assert_eq!(parsed.decisions[0].text, "Ship");

        let bare_fence = "```\n{\"decisions\":[]}\n```";
        let parsed: DecisionsWrapper = OpenAiCompatBackend::parse_json(bare_fence).unwrap();
        assert!(parsed.decisions.is_empty());
    }

    #[test]
    fn prose_instead_of_json_is_reported_clearly() {
        let err = OpenAiCompatBackend::parse_json::<DecisionsWrapper>(
            "I'm sorry, I can't help with that.",
        )
        .unwrap_err();
        assert!(matches!(err, AiError::MalformedResponse { .. }));
    }

    #[test]
    fn a_content_filter_finish_reason_deserializes() {
        let completion: Completion = serde_json::from_value(json!({
            "choices": [{
                "message": { "content": null },
                "finish_reason": "content_filter"
            }]
        }))
        .unwrap();

        assert_eq!(
            completion.choices[0].finish_reason.as_deref(),
            Some("content_filter")
        );
    }

    #[test]
    fn a_normal_completion_deserializes() {
        let completion: Completion = serde_json::from_value(json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "the answer" },
                "finish_reason": "stop"
            }],
            "usage": { "total_tokens": 42 }
        }))
        .unwrap();

        assert_eq!(
            completion.choices[0].message.content.as_deref(),
            Some("the answer")
        );
    }

    #[test]
    fn the_model_id_is_namespaced_by_provider_in_output() {
        // Two providers can serve the same model name; a stored summary should record
        // which one actually produced it.
        let backend = groq().with_model("llama-3.3-70b-versatile");
        assert_eq!(
            format!("{}/{}", backend.preset.name(), backend.model),
            "groq/llama-3.3-70b-versatile"
        );
    }

    #[tokio::test]
    async fn chat_rejects_a_malformed_history_without_a_network_call() {
        let backend = OpenAiCompatBackend::new(
            Preset::Custom {
                name: "unreachable".into(),
                base_url: "http://127.0.0.1:1".into(),
            },
            None,
        )
        .unwrap();

        let request = ChatRequest::new(vec![crate::types::ChatMessage::assistant("hi")]);
        assert!(matches!(
            backend.chat(&request).await.unwrap_err(),
            AiError::InvalidRequest(_)
        ));
    }
}
