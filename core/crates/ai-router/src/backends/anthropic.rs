//! Bring-your-own-key backend for the Anthropic Messages API.
//!
//! The user supplies their own API key; Notewise never proxies these calls. This is the
//! middle option between fully local (Ollama) and Notewise-hosted inference.
//!
//! # Three API details that are easy to get wrong
//!
//! 1. **No sampling parameters.** `temperature`, `top_p`, and `top_k` are rejected with a
//!    400 on current models. The request body below deliberately omits them; steer behaviour
//!    through the prompt instead.
//! 2. **A refusal is an HTTP 200.** Safety classifiers can decline a request and return
//!    `stop_reason: "refusal"` with an empty `content` array. Indexing `content[0]` without
//!    checking `stop_reason` first panics on exactly the responses you least want to crash on.
//! 3. **`content` is a heterogeneous block list.** Thinking blocks can precede text blocks,
//!    so the text must be located by `type`, never by position.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{AiError, Result};
use crate::types::{
    ChatRequest, ChatResponse, ExtractedActionItem, ExtractedDecision, Role, SummaryOutput,
    TranscriptInput,
};
use crate::AiBackend;

const BACKEND: &str = "anthropic";
const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

/// Pinned API version. This is a required header and is independent of the model.
const API_VERSION: &str = "2023-06-01";

/// Default model.
///
/// Fixed ID with no date suffix — appending one produces a 404.
const DEFAULT_MODEL: &str = "claude-opus-5";

/// Output ceiling. Kept below the point where a non-streaming request risks an HTTP timeout;
/// summaries and extractions land far under it.
const DEFAULT_MAX_TOKENS: u32 = 16_000;

#[derive(Debug, Clone)]
pub struct AnthropicBackend {
    api_key: String,
    model: String,
    endpoint: String,
    max_tokens: u32,
    http: reqwest::Client,
}

impl AnthropicBackend {
    /// Create a backend from a user-supplied API key.
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(AiError::MissingApiKey { backend: BACKEND });
        }

        Ok(Self {
            api_key,
            model: DEFAULT_MODEL.to_string(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            http: reqwest::Client::new(),
        })
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Override the endpoint. Intended for proxies and for pointing tests at a local stub.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Build a request body.
    ///
    /// Note what is absent: no `temperature`, `top_p`, or `top_k`. Current models reject all
    /// three with a 400.
    fn body(&self, system: &str, messages: Value, output_schema: Option<Value>) -> Value {
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": system,
            "messages": messages,
        });

        // Structured outputs constrain the reply to a schema, which replaces the older
        // trick of prefilling an assistant turn with `{` — that now returns a 400.
        if let Some(schema) = output_schema {
            body["output_config"] = json!({
                "format": { "type": "json_schema", "schema": schema }
            });
        }

        body
    }

    async fn send(&self, body: Value) -> Result<MessagesResponse> {
        let response = self
            .http
            .post(&self.endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|source| AiError::Transport {
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
                .unwrap_or(60);
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

        let parsed: MessagesResponse =
            response
                .json()
                .await
                .map_err(|source| AiError::Transport {
                    backend: BACKEND,
                    source,
                })?;

        // A refusal arrives as a successful HTTP response. Check it before reading content.
        if parsed.stop_reason.as_deref() == Some("refusal") {
            return Err(AiError::Refused {
                backend: BACKEND,
                category: parsed.stop_details.and_then(|d| d.category),
            });
        }

        Ok(parsed)
    }

    /// Send a prompt and return the reply's text.
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let body = self.body(system, json!([{ "role": "user", "content": user }]), None);
        let response = self.send(body).await?;
        response.text().ok_or_else(|| AiError::MalformedResponse {
            backend: BACKEND,
            reason: "response contained no text block".into(),
        })
    }

    /// Send a prompt constrained to a JSON schema and deserialize the reply.
    async fn complete_structured<T: for<'de> Deserialize<'de>>(
        &self,
        system: &str,
        user: &str,
        schema: Value,
    ) -> Result<T> {
        let body = self.body(
            system,
            json!([{ "role": "user", "content": user }]),
            Some(schema),
        );
        let response = self.send(body).await?;
        let text = response.text().ok_or_else(|| AiError::MalformedResponse {
            backend: BACKEND,
            reason: "response contained no text block".into(),
        })?;

        serde_json::from_str(&text).map_err(|e| AiError::MalformedResponse {
            backend: BACKEND,
            reason: format!("schema-constrained reply did not deserialize: {e}"),
        })
    }
}

/// Optional nullable string, spelled the way the schema validator accepts.
fn nullable_string() -> Value {
    json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] })
}

fn decisions_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "decisions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" },
                        "reasoning": nullable_string(),
                    },
                    "required": ["text", "reasoning"],
                    "additionalProperties": false,
                }
            }
        },
        "required": ["decisions"],
        "additionalProperties": false,
    })
}

fn action_items_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action_items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" },
                        "owner": nullable_string(),
                        "due_hint": nullable_string(),
                    },
                    "required": ["text", "owner", "due_hint"],
                    "additionalProperties": false,
                }
            }
        },
        "required": ["action_items"],
        "additionalProperties": false,
    })
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
impl AiBackend for AnthropicBackend {
    fn model_id(&self) -> &str {
        &self.model
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn summarize(&self, input: &TranscriptInput) -> Result<SummaryOutput> {
        let text = self
            .complete(
                "Summarize this meeting transcript. Lead with the outcome. Cover what was \
                 decided and what happens next. Omit small talk and scheduling chatter.",
                &transcript_prompt(input),
            )
            .await?;

        Ok(SummaryOutput {
            text,
            model: self.model.clone(),
        })
    }

    async fn extract_decisions(&self, input: &TranscriptInput) -> Result<Vec<ExtractedDecision>> {
        let wrapper: DecisionsWrapper = self
            .complete_structured(
                "Extract the decisions reached in this meeting. A decision is something \
                 settled, not a task to be done. Include the stated reasoning when the \
                 transcript makes it recoverable, and null when it does not. If nothing was \
                 decided, return an empty list rather than inventing one.",
                &transcript_prompt(input),
                decisions_schema(),
            )
            .await?;
        Ok(wrapper.decisions)
    }

    async fn extract_action_items(
        &self,
        input: &TranscriptInput,
    ) -> Result<Vec<ExtractedActionItem>> {
        let wrapper: ActionItemsWrapper = self
            .complete_structured(
                "Extract the action items from this meeting — work someone committed to \
                 doing. Record the owner exactly as named in the transcript, and any due date \
                 exactly as stated (e.g. 'next Friday') without resolving it to a calendar \
                 date. Use null where the transcript does not say. Return an empty list if \
                 there are none.",
                &transcript_prompt(input),
                action_items_schema(),
            )
            .await?;
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
             If the material does not contain the answer, say so plainly rather than guessing.",
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

        let response = self.send(self.body(&system, json!(messages), None)).await?;
        let text = response.text().ok_or_else(|| AiError::MalformedResponse {
            backend: BACKEND,
            reason: "response contained no text block".into(),
        })?;

        Ok(ChatResponse {
            text,
            model: response.model.unwrap_or_else(|| self.model.clone()),
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

#[derive(Debug, Deserialize, Serialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_details: Option<StopDetails>,
    #[serde(default)]
    model: Option<String>,
}

impl MessagesResponse {
    /// Concatenate the text blocks.
    ///
    /// Located by `type`, never by position — thinking blocks can precede the text.
    fn text(&self) -> Option<String> {
        let text: String = self
            .content
            .iter()
            .filter(|b| b.kind == "text")
            .filter_map(|b| b.text.as_deref())
            .collect::<Vec<_>>()
            .join("");

        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct StopDetails {
    #[serde(default)]
    category: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> AnthropicBackend {
        AnthropicBackend::new("sk-ant-test-key").expect("valid key")
    }

    #[test]
    fn rejects_an_empty_api_key() {
        assert!(matches!(
            AnthropicBackend::new("   ").expect_err("blank key"),
            AiError::MissingApiKey { .. }
        ));
    }

    #[test]
    fn defaults_to_a_current_model_with_no_date_suffix() {
        let backend = backend();
        assert_eq!(backend.model_id(), "claude-opus-5");
        assert!(
            !backend.model_id().contains("-2025") && !backend.model_id().contains("-2026"),
            "appending a date suffix to this model id produces a 404"
        );
    }

    #[test]
    fn is_not_local() {
        assert!(!backend().is_local(), "BYOK sends transcripts off-device");
    }

    #[test]
    fn request_body_omits_sampling_parameters() {
        // temperature / top_p / top_k are rejected with a 400 on current models.
        let body = backend().body("system", json!([]), None);

        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert!(body.get("top_k").is_none());
    }

    #[test]
    fn request_body_carries_the_required_fields() {
        let body = backend().body("be concise", json!([{"role": "user", "content": "hi"}]), None);

        assert_eq!(body["model"], "claude-opus-5");
        assert_eq!(body["max_tokens"], 16_000);
        assert_eq!(body["system"], "be concise");
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn structured_requests_carry_the_schema_not_a_prefill() {
        // Prefilling a trailing assistant turn returns a 400 on current models;
        // output_config is the supported replacement.
        let body = backend().body("system", json!([]), Some(decisions_schema()));

        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert!(body["output_config"]["format"]["schema"]["properties"]["decisions"].is_object());

        let messages = body["messages"].as_array().unwrap();
        assert!(
            messages.iter().all(|m| m["role"] != "assistant"),
            "must not prefill an assistant turn"
        );
    }

    #[test]
    fn schemas_close_every_object() {
        // The validator requires additionalProperties: false on every object.
        for schema in [decisions_schema(), action_items_schema()] {
            assert_eq!(schema["additionalProperties"], false);
            let items = schema["properties"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap()["items"]
                .clone();
            assert_eq!(items["additionalProperties"], false);
        }
    }

    #[test]
    fn builders_override_defaults() {
        let backend = backend()
            .with_model("claude-sonnet-5")
            .with_max_tokens(4096)
            .with_endpoint("http://localhost:9999/v1/messages");

        assert_eq!(backend.model_id(), "claude-sonnet-5");
        assert_eq!(backend.body("s", json!([]), None)["max_tokens"], 4096);
        assert_eq!(backend.endpoint, "http://localhost:9999/v1/messages");
    }

    #[test]
    fn text_is_found_by_block_type_not_position() {
        // A thinking block can precede the text; indexing content[0] would return nothing.
        let response: MessagesResponse = serde_json::from_value(json!({
            "content": [
                { "type": "thinking", "thinking": "..." },
                { "type": "text", "text": "the answer" }
            ],
            "stop_reason": "end_turn",
        }))
        .unwrap();

        assert_eq!(response.text().as_deref(), Some("the answer"));
    }

    #[test]
    fn multiple_text_blocks_are_concatenated() {
        let response: MessagesResponse = serde_json::from_value(json!({
            "content": [
                { "type": "text", "text": "first " },
                { "type": "text", "text": "second" }
            ],
        }))
        .unwrap();

        assert_eq!(response.text().as_deref(), Some("first second"));
    }

    #[test]
    fn empty_content_yields_no_text_rather_than_panicking() {
        // This is the shape a refusal arrives in.
        let response: MessagesResponse = serde_json::from_value(json!({
            "content": [],
            "stop_reason": "refusal",
        }))
        .unwrap();

        assert_eq!(response.text(), None);
    }

    #[test]
    fn refusal_shape_deserializes_with_its_category() {
        let response: MessagesResponse = serde_json::from_value(json!({
            "content": [],
            "stop_reason": "refusal",
            "stop_details": { "type": "refusal", "category": "cyber" },
        }))
        .unwrap();

        assert_eq!(response.stop_reason.as_deref(), Some("refusal"));
        assert_eq!(
            response.stop_details.and_then(|d| d.category).as_deref(),
            Some("cyber")
        );
    }

    #[test]
    fn responses_without_stop_details_deserialize() {
        // stop_details is null for every stop_reason other than refusal.
        let response: MessagesResponse = serde_json::from_value(json!({
            "content": [{ "type": "text", "text": "hi" }],
            "stop_reason": "end_turn",
            "model": "claude-opus-5",
        }))
        .unwrap();

        assert!(response.stop_details.is_none());
        assert_eq!(response.model.as_deref(), Some("claude-opus-5"));
    }

    #[test]
    fn transcript_prompt_includes_context_when_present() {
        let with = transcript_prompt(
            &TranscriptInput::new("Sync", "text").with_context("Project Apollo"),
        );
        let without = transcript_prompt(&TranscriptInput::new("Sync", "text"));

        assert!(with.contains("Project Apollo"));
        assert!(!without.contains("Context:"));
    }

    #[tokio::test]
    async fn chat_rejects_a_malformed_history_without_a_network_call() {
        let backend = backend().with_endpoint("http://127.0.0.1:1/unreachable");
        let request = ChatRequest::new(vec![crate::types::ChatMessage::assistant("hello")]);

        let err = backend.chat(&request).await.expect_err("should be rejected");
        assert!(
            matches!(err, AiError::InvalidRequest(_)),
            "expected local validation, not a transport error: {err:?}"
        );
    }

    #[test]
    fn api_version_header_is_pinned() {
        assert_eq!(API_VERSION, "2023-06-01");
    }
}
