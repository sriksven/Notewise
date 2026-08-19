//! Local inference through an Ollama daemon.
//!
//! This is the fully-local option: transcripts never leave the machine. It requires the user
//! to be running Ollama and to have pulled a model.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::embed::is_embedding_model;
use crate::error::{AiError, Result};
use crate::tags;
use crate::types::{
    ChatRequest, ChatResponse, ExtractedActionItem, ExtractedDecision, Role, SummaryOutput,
    TranscriptInput,
};
use crate::AiBackend;

const BACKEND: &str = "ollama";
const DEFAULT_ENDPOINT: &str = "http://localhost:11434/api/chat";
/// The preferred model when nobody has chosen one.
///
/// A *preference*, not an assertion. Ollama expands an untagged name to `:latest`, so this
/// string alone is a claim that the user pulled `llama3.1:latest` specifically — and it is a
/// 404 on a machine holding `llama3.1:8b`. It is resolved against the daemon's actual model
/// list before any request; see [`OllamaBackend::resolve`] and `crate::tags`.
const DEFAULT_MODEL: &str = "llama3.1";

/// Whether a failed response means the configured model is absent, rather than the daemon
/// being unwell.
///
/// Pure, and separate from the request path, for two reasons: it is the part that can be wrong,
/// and the async path should only pay for a model list when the answer is yes.
fn is_model_missing(status: u16, body: &str) -> bool {
    status == 404 && body.to_ascii_lowercase().contains("not found")
}

#[derive(Debug, Clone)]
pub struct OllamaBackend {
    /// The model asked for: either the user's choice or [`DEFAULT_MODEL`].
    model: String,
    /// Whether a human picked `model`.
    ///
    /// The difference decides what happens when it is not installed. A name the user chose
    /// must fail loudly — substituting another model would attribute output to a choice they
    /// did not make. Our own default may fall back to whatever the daemon holds, because
    /// "llama3.1 is missing" is not a useful thing to tell someone who never asked for it.
    chosen: bool,
    /// The tag actually sent, resolved once against the daemon's model list.
    ///
    /// Shared across clones rather than per-clone, so the resolution costs one `/api/tags`
    /// per backend rather than one per caller that happened to clone it.
    resolved: Arc<tokio::sync::OnceCell<String>>,
    endpoint: String,
    http: reqwest::Client,
}

impl Default for OllamaBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl OllamaBackend {
    pub fn new() -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            chosen: false,
            resolved: Arc::new(tokio::sync::OnceCell::new()),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self.chosen = true;
        self
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Build a request body.
    ///
    /// `stream: false` matters — Ollama streams by default, and a streaming response does not
    /// deserialize as a single JSON object.
    fn body(&self, model: &str, messages: Value, json_mode: bool) -> Value {
        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": false,
        });

        if json_mode {
            // Ollama's format flag constrains output to valid JSON. It does not enforce a
            // schema the way structured outputs do, so the shape is still validated on parse.
            body["format"] = json!("json");
        }

        body
    }

    async fn send(&self, messages: Value, json_mode: bool) -> Result<ChatCompletion> {
        let model = self.resolve().await;
        let response = self
            .http
            .post(&self.endpoint)
            .json(&self.body(&model, messages, json_mode))
            .send()
            .await
            .map_err(|source| AiError::Transport {
                backend: BACKEND,
                source,
            })?;

        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable error body>".into());

            if is_model_missing(status.as_u16(), &message) {
                // Best effort. If the daemon cannot even list its models we would be replacing
                // one unhelpful error with a different one, so an empty list still produces a
                // message that names the configured model and says to pull something.
                let installed = self.installed_models().await.unwrap_or_default();
                return Err(AiError::ModelNotInstalled {
                    backend: BACKEND,
                    // The tag that was actually sent, not the name it was resolved from.
                    // Naming a model the daemon never saw would send the user looking for the
                    // wrong thing.
                    model,
                    installed,
                });
            }

            return Err(AiError::Provider {
                backend: BACKEND,
                status: status.as_u16(),
                message,
            });
        }

        response.json().await.map_err(|source| AiError::Transport {
            backend: BACKEND,
            source,
        })
    }

    async fn complete(&self, system: &str, user: &str, json_mode: bool) -> Result<String> {
        let messages = json!([
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ]);
        let completion = self.send(messages, json_mode).await?;
        Ok(completion.message.content)
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
impl AiBackend for OllamaBackend {
    /// The tag in use, once it is known.
    ///
    /// Before the first request this is the *preferred* name, because nothing has asked the
    /// daemon yet. Afterwards it is the tag that actually answered, which is what belongs
    /// beside stored output.
    fn model_id(&self) -> &str {
        self.resolved
            .get()
            .map(String::as_str)
            .unwrap_or(&self.model)
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn summarize(&self, input: &TranscriptInput) -> Result<SummaryOutput> {
        let text = self
            .complete(
                input.system_prompt(
                    "Summarize this meeting transcript. Lead with the outcome. Cover what was \
                 decided and what happens next. Omit small talk.",
                ),
                &transcript_prompt(input),
                false,
            )
            .await?;

        Ok(SummaryOutput {
            text,
            // After `complete`, this is the resolved tag rather than the preference.
            model: self.model_id().to_string(),
        })
    }

    async fn extract_decisions(&self, input: &TranscriptInput) -> Result<Vec<ExtractedDecision>> {
        let raw = self
            .complete(
                "Extract the decisions reached in this meeting. Reply with JSON of the form \
                 {\"decisions\": [{\"text\": \"...\", \"reasoning\": \"...\" or null}]}. \
                 Return an empty list if nothing was decided.",
                &transcript_prompt(input),
                true,
            )
            .await?;

        let wrapper: DecisionsWrapper =
            serde_json::from_str(&raw).map_err(|e| AiError::MalformedResponse {
                backend: BACKEND,
                reason: format!("model did not return the expected decision shape: {e}"),
            })?;
        Ok(wrapper.decisions)
    }

    async fn extract_action_items(
        &self,
        input: &TranscriptInput,
    ) -> Result<Vec<ExtractedActionItem>> {
        let raw = self
            .complete(
                "Extract the action items from this meeting. Reply with JSON of the form \
                 {\"action_items\": [{\"text\": \"...\", \"owner\": \"...\" or null, \
                 \"due_hint\": \"...\" or null}]}. Keep due dates exactly as stated. \
                 Return an empty list if there are none.",
                &transcript_prompt(input),
                true,
            )
            .await?;

        let wrapper: ActionItemsWrapper =
            serde_json::from_str(&raw).map_err(|e| AiError::MalformedResponse {
                backend: BACKEND,
                reason: format!("model did not return the expected action item shape: {e}"),
            })?;
        Ok(wrapper.action_items)
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

        let mut messages = vec![json!({ "role": "system", "content": system })];
        messages.extend(request.messages.iter().map(|m| {
            json!({
                "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                "content": m.content,
            })
        }));

        let completion = self.send(json!(messages), false).await?;
        Ok(ChatResponse {
            text: completion.message.content,
            model: completion
                .model
                .unwrap_or_else(|| self.model_id().to_string()),
        })
    }

    async fn resolved_model_id(&self) -> String {
        self.resolve().await
    }

    /// Ask the daemon for its model list.
    ///
    /// `/api/tags` is a cheap GET that loads no model. The configured endpoint points at
    /// `/api/chat`, so the base is recovered by trimming that suffix rather than by storing a
    /// second URL that could drift out of sync with the first.
    async fn probe(&self) -> Result<()> {
        self.tags().await.map(|_| ())
    }

    async fn installed_models(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();

        for entry in self.tags().await?.models {
            // Embedding models are installed alongside chat models and cannot answer a
            // question. Offering `nomic-embed-text` in a model picker is offering a choice
            // that fails on the next summary, with an error about the model rather than about
            // the choice.
            if self.can_generate(&entry.name).await {
                names.push(entry.name);
            }
        }

        // Sorted so the picker does not reorder itself between launches; Ollama returns them
        // by modification time, which changes every time a model is used.
        names.sort();
        Ok(names)
    }
}

impl OllamaBackend {
    /// The tag to send, resolved once against what the daemon actually holds.
    ///
    /// Costs one `/api/tags` on the first request of a process and nothing afterwards. That
    /// is the whole price of never shipping a default that names a model the user does not
    /// have — the daemon was already being asked this question to build the model picker.
    async fn resolve(&self) -> String {
        self.resolved
            .get_or_init(|| self.resolve_uncached())
            .await
            .clone()
    }

    async fn resolve_uncached(&self) -> String {
        let Ok(installed) = self.installed_models().await else {
            // The daemon is unreachable or unwell. Sending the preferred name produces a
            // transport error that names the daemon, which is the accurate complaint;
            // inventing a model here would replace it with a misleading one.
            return self.model.clone();
        };

        if let Some(tag) = tags::resolve_tag(&self.model, &installed) {
            return tag;
        }

        if self.chosen {
            // Their choice, and it is not installed. Sending it unchanged gets
            // `ModelNotInstalled`, which names what *is* installed — a better answer than
            // silently running a model they did not pick.
            return self.model.clone();
        }

        // Nobody chose this and the preferred family is absent. A machine with only `mistral`
        // should summarize the meeting rather than report that our preference is missing.
        tags::first_acceptable(&installed, |model| !is_embedding_model(model))
            .unwrap_or_else(|| self.model.clone())
    }

    /// The daemon's base URL, recovered from the configured chat endpoint.
    fn base(&self) -> &str {
        self.endpoint
            .strip_suffix("/api/chat")
            .unwrap_or(&self.endpoint)
    }

    /// Whether this model can hold a conversation, as opposed to producing embeddings.
    ///
    /// Fails open. Older daemons do not report capabilities at all, and hiding every model
    /// from someone running an older Ollama would be a worse outcome than occasionally listing
    /// one that turns out to be an embedder.
    async fn can_generate(&self, model: &str) -> bool {
        let response = self
            .http
            .post(format!("{}/api/show", self.base()))
            .json(&json!({ "model": model }))
            // Metadata only — this loads nothing, so a second is generous.
            .timeout(std::time::Duration::from_secs(1))
            .send()
            .await;

        let Ok(response) = response else { return true };
        if !response.status().is_success() {
            return true;
        }

        match response.json::<ShowResponse>().await {
            Ok(show) => match show.capabilities {
                Some(caps) if !caps.is_empty() => caps.iter().any(|c| c == "completion"),
                _ => true,
            },
            Err(_) => true,
        }
    }

    /// Ask the daemon what it holds.
    ///
    /// `/api/tags` is a cheap GET that loads no model. The configured endpoint points at
    /// `/api/chat`, so the base is recovered by trimming that suffix rather than by storing a
    /// second URL that could drift out of sync with the first.
    async fn tags(&self) -> Result<TagsResponse> {
        let response = self
            .http
            .get(format!("{}/api/tags", self.base()))
            // Bounded, because this runs on the setup screen's critical path: an installed but
            // stopped daemon must answer "not reachable" in a moment rather than hang until
            // the OS connect timeout expires.
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .map_err(|source| AiError::Transport {
                backend: BACKEND,
                source,
            })?;

        if !response.status().is_success() {
            return Err(AiError::Provider {
                backend: BACKEND,
                status: response.status().as_u16(),
                message: "the Ollama daemon did not return its model list".into(),
            });
        }

        response
            .json::<TagsResponse>()
            .await
            .map_err(|source| AiError::Transport {
                backend: BACKEND,
                source,
            })
    }
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagEntry>,
}

#[derive(Debug, Deserialize)]
struct TagEntry {
    /// The exact tag, such as `llama3.1:8b`. This is what has to be sent back as the model.
    name: String,
}

#[derive(Debug, Deserialize)]
struct ShowResponse {
    /// `["completion", …]` or `["embedding"]`. Absent on older daemons.
    capabilities: Option<Vec<String>>,
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
struct ChatCompletion {
    message: CompletionMessage,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CompletionMessage {
    content: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChatMessage;

    #[test]
    fn defaults_point_at_the_local_daemon() {
        let backend = OllamaBackend::new();
        assert_eq!(backend.endpoint, "http://localhost:11434/api/chat");
        assert_eq!(backend.model_id(), "llama3.1");
    }

    /// The bug this predicate exists for: Ollama resolves a bare `llama3.1` to
    /// `llama3.1:latest`, so a machine holding only `llama3.1:8b` answers 404.
    #[test]
    fn a_404_naming_a_missing_model_is_recognised() {
        assert!(is_model_missing(
            404,
            r#"{"error":"model 'llama3.1' not found"}"#
        ));
    }

    #[test]
    fn other_failures_are_not_mistaken_for_a_missing_model() {
        // A sick daemon must not be reported as a model choice problem: the user would go
        // change a setting that was never wrong.
        assert!(!is_model_missing(500, "internal error"));
        assert!(!is_model_missing(404, "endpoint does not exist"));
        assert!(!is_model_missing(200, "model 'x' not found"));
    }

    #[test]
    fn the_predicate_does_not_care_about_case() {
        assert!(is_model_missing(404, r#"{"error":"Model 'x' NOT FOUND"}"#));
    }

    /// The message is what reaches the user — it is rendered verbatim in the Ask panel — so
    /// it has to name both the model that failed and the ones that would work.
    #[test]
    fn the_missing_model_error_names_the_alternatives() {
        let err = AiError::ModelNotInstalled {
            backend: BACKEND,
            model: "llama3.1".into(),
            installed: vec!["llama3.1:8b".into(), "llama3:latest".into()],
        };

        let shown = err.to_string();
        assert!(shown.contains("llama3.1:8b"), "{shown}");
        assert!(shown.contains("Pick one in Settings"), "{shown}");
    }

    #[test]
    fn with_nothing_installed_it_says_to_pull_something() {
        let err = AiError::ModelNotInstalled {
            backend: BACKEND,
            model: "llama3.1".into(),
            installed: Vec::new(),
        };

        assert!(err.to_string().contains("ollama pull"), "{err}");
    }

    /// Proves the fix against a real daemon: a bare `llama3.1` on a machine that holds
    /// `llama3.1:8b` must come back naming the installed tags, not as a raw 404.
    ///
    /// `#[ignore]`d because it needs a running Ollama with at least one chat model pulled,
    /// which a CI runner does not have. Run with
    /// `cargo test -p notewise-ai-router -- --ignored missing_model_against_a_real_daemon`.
    #[tokio::test]
    #[ignore = "needs a running Ollama daemon with a chat model pulled"]
    async fn missing_model_against_a_real_daemon() {
        let backend = OllamaBackend::new().with_model("definitely-not-a-real-model");
        let err = backend
            .summarize(&TranscriptInput::new("t", "we agreed to ship"))
            .await
            .expect_err("a model that does not exist cannot summarize");

        match err {
            AiError::ModelNotInstalled { installed, .. } => {
                assert!(
                    !installed.is_empty(),
                    "the daemon should have reported what it does hold"
                );
            }
            other => panic!("expected ModelNotInstalled, got {other:?}"),
        }
    }

    #[test]
    fn reports_itself_as_local() {
        assert!(
            OllamaBackend::new().is_local(),
            "this is the whole point of the Ollama backend"
        );
    }

    #[test]
    fn streaming_is_disabled() {
        // Ollama streams by default, and a streamed body does not deserialize as one object.
        let body = OllamaBackend::new().body("llama3.1:8b", json!([]), false);
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn json_mode_is_opt_in() {
        let backend = OllamaBackend::new();
        assert!(backend
            .body("llama3.1:8b", json!([]), false)
            .get("format")
            .is_none());
        assert_eq!(
            backend.body("llama3.1:8b", json!([]), true)["format"],
            "json"
        );
    }

    #[test]
    fn the_shipped_default_is_a_preference_rather_than_a_claim() {
        // `new()` names a model nobody asked for, so it may be resolved against the daemon or
        // fall back. `with_model` records a decision, and a decision is not ours to override.
        assert!(!OllamaBackend::new().chosen);
        assert!(OllamaBackend::new().with_model("mistral:7b").chosen);
    }

    #[test]
    fn the_reported_model_is_the_preference_until_something_resolves_it() {
        // Nothing has asked the daemon yet, so the honest answer is what was asked for.
        assert_eq!(OllamaBackend::new().model_id(), DEFAULT_MODEL);
        assert_eq!(
            OllamaBackend::new().with_model("mistral:7b").model_id(),
            "mistral:7b"
        );
    }

    #[test]
    fn builders_override_defaults() {
        let backend = OllamaBackend::new()
            .with_model("mistral")
            .with_endpoint("http://192.168.1.10:11434/api/chat");

        assert_eq!(backend.model_id(), "mistral");
        assert_eq!(backend.endpoint, "http://192.168.1.10:11434/api/chat");
    }

    #[test]
    fn completion_response_deserializes() {
        let completion: ChatCompletion = serde_json::from_value(json!({
            "model": "llama3.1",
            "message": { "role": "assistant", "content": "the answer" },
            "done": true,
        }))
        .unwrap();

        assert_eq!(completion.message.content, "the answer");
        assert_eq!(completion.model.as_deref(), Some("llama3.1"));
    }

    #[tokio::test]
    async fn chat_rejects_a_malformed_history_without_a_network_call() {
        let backend = OllamaBackend::new().with_endpoint("http://127.0.0.1:1/unreachable");
        let request = ChatRequest::new(vec![ChatMessage::assistant("hello")]);

        let err = backend
            .chat(&request)
            .await
            .expect_err("should be rejected");
        assert!(
            matches!(err, AiError::InvalidRequest(_)),
            "expected local validation, not a transport error: {err:?}"
        );
    }

    /// A refused connection is a reachability answer, not a panic. Port 1 is reserved and
    /// nothing listens on it, so this exercises the failure path without a network.
    #[tokio::test]
    async fn probe_reports_an_unreachable_daemon() {
        let backend = OllamaBackend::new().with_endpoint("http://127.0.0.1:1/api/chat");
        let err = backend
            .probe()
            .await
            .expect_err("nothing listens on port 1");
        assert!(matches!(err, AiError::Transport { .. }), "got {err:?}");
    }

    #[tokio::test]
    #[ignore = "requires a running Ollama daemon with a pulled model"]
    async fn summarizes_against_a_live_daemon() {
        let backend = OllamaBackend::new();
        let input = TranscriptInput::new("Standup", "Alex: we agreed to ship on Friday.");
        let summary = backend.summarize(&input).await.expect("live daemon");
        assert!(!summary.text.is_empty());
    }
}
