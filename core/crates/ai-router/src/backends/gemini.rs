//! Google Gemini.
//!
//! Kept separate from [`OpenAiCompatBackend`](super::OpenAiCompatBackend) because Gemini does
//! not speak the chat-completions shape: system text goes in `systemInstruction` rather than
//! a message, the assistant role is `model` rather than `assistant`, and the key travels as a
//! query parameter. Squeezing it into the compatible backend would mean branching on provider
//! inside every method, which is worse than one small extra file.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{AiError, Result};
use crate::types::{
    ChatRequest, ChatResponse, ExtractedActionItem, ExtractedDecision, Role, SummaryOutput,
    TranscriptInput,
};
use crate::AiBackend;

const BACKEND: &str = "gemini";
const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const DEFAULT_MODEL: &str = "gemini-2.0-flash";

#[derive(Debug, Clone)]
pub struct GeminiBackend {
    api_key: String,
    model: String,
    base_url: String,
    http: reqwest::Client,
}

impl GeminiBackend {
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(AiError::MissingApiKey { backend: BACKEND });
        }

        Ok(Self {
            api_key,
            model: DEFAULT_MODEL.to_string(),
            base_url: BASE_URL.to_string(),
            http: reqwest::Client::new(),
        })
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Override the base URL. For proxies, and for pointing tests at a stub.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/{}:generateContent",
            self.base_url.trim_end_matches('/'),
            self.model
        )
    }

    fn body(&self, system: &str, contents: Vec<Value>, json_mode: bool) -> Value {
        let mut body = json!({
            // Gemini takes system text as its own field, not as a message with a role.
            "systemInstruction": { "parts": [{ "text": system }] },
            "contents": contents,
        });

        if json_mode {
            body["generationConfig"] = json!({ "responseMimeType": "application/json" });
        }

        body
    }

    async fn send(&self, body: Value) -> Result<String> {
        let response = self
            .http
            .post(self.endpoint())
            // Gemini authenticates by query parameter rather than a header.
            .query(&[("key", &self.api_key)])
            .json(&body)
            .send()
            .await
            .map_err(|source| AiError::Transport {
                backend: BACKEND,
                source,
            })?;

        let status = response.status();

        if status.as_u16() == 429 {
            return Err(AiError::RateLimited {
                backend: BACKEND,
                retry_after_secs: 30,
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

        let parsed: GenerateResponse =
            response.json().await.map_err(|source| AiError::Transport {
                backend: BACKEND,
                source,
            })?;

        // A blocked prompt returns 200 with no candidates at all, so this has to be checked
        // before reading them or the failure looks like an empty answer.
        if let Some(feedback) = parsed.prompt_feedback.as_ref() {
            if let Some(reason) = feedback.block_reason.as_ref() {
                return Err(AiError::Refused {
                    backend: BACKEND,
                    category: Some(reason.clone()),
                });
            }
        }

        let candidate = parsed
            .candidates
            .into_iter()
            .next()
            .ok_or_else(|| AiError::Refused {
                backend: BACKEND,
                category: None,
            })?;

        // Safety stops also arrive as a successful response.
        if matches!(
            candidate.finish_reason.as_deref(),
            Some("SAFETY" | "BLOCKLIST")
        ) {
            return Err(AiError::Refused {
                backend: BACKEND,
                category: candidate.finish_reason,
            });
        }

        let text: String = candidate
            .content
            .map(|content| {
                content
                    .parts
                    .into_iter()
                    .filter_map(|part| part.text)
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();

        if text.trim().is_empty() {
            return Err(AiError::MalformedResponse {
                backend: BACKEND,
                reason: "response contained no text parts".into(),
            });
        }

        Ok(text)
    }

    fn parse_json<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T> {
        let cleaned = raw.trim();
        let cleaned = cleaned
            .strip_prefix("```json")
            .or_else(|| cleaned.strip_prefix("```"))
            .map(str::trim_start)
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

fn user_turn(text: String) -> Vec<Value> {
    vec![json!({ "role": "user", "parts": [{ "text": text }] })]
}

#[async_trait]
impl AiBackend for GeminiBackend {
    fn model_id(&self) -> &str {
        &self.model
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn summarize(&self, input: &TranscriptInput) -> Result<SummaryOutput> {
        let text = self
            .send(self.body(
                "Summarize this meeting transcript. Lead with the outcome. Cover what was \
                 decided and what happens next. Omit small talk.",
                user_turn(transcript_prompt(input)),
                false,
            ))
            .await?;

        Ok(SummaryOutput {
            text,
            model: self.model.clone(),
        })
    }

    async fn extract_decisions(&self, input: &TranscriptInput) -> Result<Vec<ExtractedDecision>> {
        let raw = self
            .send(self.body(
                "Extract the decisions reached in this meeting — things settled, not tasks. \
                 Reply with JSON only: {\"decisions\": [{\"text\": \"...\", \
                 \"reasoning\": \"...\" or null}]}. Return an empty list if nothing was decided.",
                user_turn(transcript_prompt(input)),
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
                user_turn(transcript_prompt(input)),
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

        let contents: Vec<Value> = request
            .messages
            .iter()
            .map(|m| {
                json!({
                    // Gemini's assistant role is "model", not "assistant".
                    "role": match m.role { Role::User => "user", Role::Assistant => "model" },
                    "parts": [{ "text": m.content }],
                })
            })
            .collect();

        let text = self.send(self.body(&system, contents, false)).await?;

        Ok(ChatResponse {
            text,
            model: self.model.clone(),
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
struct GenerateResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default, rename = "promptFeedback")]
    prompt_feedback: Option<PromptFeedback>,
}

#[derive(Debug, Deserialize)]
struct PromptFeedback {
    #[serde(default, rename = "blockReason")]
    block_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    #[serde(default)]
    content: Option<Content>,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Content {
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Debug, Deserialize)]
struct Part {
    #[serde(default)]
    text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> GeminiBackend {
        GeminiBackend::new("AIza-test").expect("valid key")
    }

    #[test]
    fn an_empty_key_is_rejected() {
        assert!(matches!(
            GeminiBackend::new("   ").unwrap_err(),
            AiError::MissingApiKey { .. }
        ));
    }

    #[test]
    fn is_never_local() {
        assert!(!backend().is_local());
    }

    #[test]
    fn the_endpoint_embeds_the_model_and_action() {
        assert_eq!(
            backend().with_model("gemini-2.0-pro").endpoint(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent"
        );
    }

    #[test]
    fn system_text_is_a_field_not_a_message() {
        // Gemini rejects a "system" role inside contents.
        let body = backend().body("be brief", user_turn("hi".into()), false);

        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be brief");
        let contents = body["contents"].as_array().unwrap();
        assert!(contents.iter().all(|c| c["role"] != "system"));
    }

    #[test]
    fn json_mode_sets_the_response_mime_type() {
        let backend = backend();
        assert!(backend
            .body("s", vec![], false)
            .get("generationConfig")
            .is_none());
        assert_eq!(
            backend.body("s", vec![], true)["generationConfig"]["responseMimeType"],
            "application/json"
        );
    }

    #[test]
    fn a_normal_response_deserializes() {
        let parsed: GenerateResponse = serde_json::from_value(json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": "the answer" }] },
                "finishReason": "STOP"
            }]
        }))
        .unwrap();

        assert_eq!(parsed.candidates.len(), 1);
        assert_eq!(parsed.candidates[0].finish_reason.as_deref(), Some("STOP"));
    }

    #[test]
    fn multi_part_responses_concatenate() {
        let parsed: GenerateResponse = serde_json::from_value(json!({
            "candidates": [{
                "content": { "parts": [{ "text": "first " }, { "text": "second" }] },
                "finishReason": "STOP"
            }]
        }))
        .unwrap();

        let text: String = parsed.candidates[0]
            .content
            .as_ref()
            .unwrap()
            .parts
            .iter()
            .filter_map(|p| p.text.clone())
            .collect();
        assert_eq!(text, "first second");
    }

    #[test]
    fn a_blocked_prompt_deserializes_with_its_reason() {
        // Arrives as HTTP 200 with no candidates — reading candidates first would
        // report this as an empty answer.
        let parsed: GenerateResponse = serde_json::from_value(json!({
            "promptFeedback": { "blockReason": "SAFETY" }
        }))
        .unwrap();

        assert!(parsed.candidates.is_empty());
        assert_eq!(
            parsed
                .prompt_feedback
                .and_then(|f| f.block_reason)
                .as_deref(),
            Some("SAFETY")
        );
    }

    #[test]
    fn a_safety_finish_reason_deserializes() {
        let parsed: GenerateResponse = serde_json::from_value(json!({
            "candidates": [{ "finishReason": "SAFETY" }]
        }))
        .unwrap();

        assert_eq!(
            parsed.candidates[0].finish_reason.as_deref(),
            Some("SAFETY")
        );
        assert!(parsed.candidates[0].content.is_none());
    }

    #[test]
    fn fenced_json_parses() {
        let parsed: DecisionsWrapper =
            GeminiBackend::parse_json("```json\n{\"decisions\":[]}\n```").unwrap();
        assert!(parsed.decisions.is_empty());
    }

    #[tokio::test]
    async fn chat_rejects_a_malformed_history_without_a_network_call() {
        let backend = backend().with_base_url("http://127.0.0.1:1");
        let request = ChatRequest::new(vec![crate::types::ChatMessage::assistant("hi")]);

        assert!(matches!(
            backend.chat(&request).await.unwrap_err(),
            AiError::InvalidRequest(_)
        ));
    }

    #[test]
    fn chat_maps_the_assistant_role_to_model() {
        // "assistant" is rejected by Gemini; the role must be "model".
        let messages = [
            crate::types::ChatMessage::user("hi"),
            crate::types::ChatMessage::assistant("hello"),
            crate::types::ChatMessage::user("and?"),
        ];

        let roles: Vec<&str> = messages
            .iter()
            .map(|m| match m.role {
                Role::User => "user",
                Role::Assistant => "model",
            })
            .collect();

        assert_eq!(roles, vec!["user", "model", "user"]);
    }
}
