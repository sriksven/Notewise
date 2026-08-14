//! Local inference through an Ollama daemon.
//!
//! This is the fully-local option: transcripts never leave the machine. It requires the user
//! to be running Ollama and to have pulled a model.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{AiError, Result};
use crate::types::{
    ChatRequest, ChatResponse, ExtractedActionItem, ExtractedDecision, Role, SummaryOutput,
    TranscriptInput,
};
use crate::AiBackend;

const BACKEND: &str = "ollama";
const DEFAULT_ENDPOINT: &str = "http://localhost:11434/api/chat";
const DEFAULT_MODEL: &str = "llama3.1";

#[derive(Debug, Clone)]
pub struct OllamaBackend {
    model: String,
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
            endpoint: DEFAULT_ENDPOINT.to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
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
    fn body(&self, messages: Value, json_mode: bool) -> Value {
        let mut body = json!({
            "model": self.model,
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

    async fn send(&self, body: Value) -> Result<ChatCompletion> {
        let response = self
            .http
            .post(&self.endpoint)
            .json(&body)
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
        let completion = self.send(self.body(messages, json_mode)).await?;
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
    fn model_id(&self) -> &str {
        &self.model
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn summarize(&self, input: &TranscriptInput) -> Result<SummaryOutput> {
        let text = self
            .complete(
                "Summarize this meeting transcript. Lead with the outcome. Cover what was \
                 decided and what happens next. Omit small talk.",
                &transcript_prompt(input),
                false,
            )
            .await?;

        Ok(SummaryOutput {
            text,
            model: self.model.clone(),
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

        let completion = self.send(self.body(json!(messages), false)).await?;
        Ok(ChatResponse {
            text: completion.message.content,
            model: completion.model.unwrap_or_else(|| self.model.clone()),
        })
    }

    /// Ask the daemon for its model list.
    ///
    /// `/api/tags` is a cheap GET that loads no model. The configured endpoint points at
    /// `/api/chat`, so the base is recovered by trimming that suffix rather than by storing a
    /// second URL that could drift out of sync with the first.
    async fn probe(&self) -> Result<()> {
        let base = self
            .endpoint
            .strip_suffix("/api/chat")
            .unwrap_or(&self.endpoint);

        let response = self
            .http
            .get(format!("{base}/api/tags"))
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

        Ok(())
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
        let body = OllamaBackend::new().body(json!([]), false);
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn json_mode_is_opt_in() {
        let backend = OllamaBackend::new();
        assert!(backend.body(json!([]), false).get("format").is_none());
        assert_eq!(backend.body(json!([]), true)["format"], "json");
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
