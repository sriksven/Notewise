//! Deterministic in-process backend.
//!
//! This is not a test fixture that leaked into the public API — it is what keeps the
//! `AiBackend` seam testable. Without it, every test touching summarization would need a GPU
//! or a paid API key, those tests would get skipped, and the boundary would erode. It is also
//! genuinely useful for UI development and demos with no model available.

use async_trait::async_trait;

use crate::error::{AiError, Result};
use crate::types::{
    ChatRequest, ChatResponse, ExtractedActionItem, ExtractedDecision, SummaryOutput,
    TranscriptInput,
};
use crate::AiBackend;

/// Backend that produces deterministic output derived from its input.
#[derive(Debug, Clone, Default)]
pub struct MockBackend {
    /// When set, every method returns this message as an `InvalidRequest` error.
    /// Lets callers exercise their own failure handling without a live provider.
    failure: Option<String>,
    /// When set, every method fails with this error instead. Separate from `failure` because
    /// that one is always `InvalidRequest`, which is not retryable — and a caller testing its
    /// own retry path needs an error that is.
    retryable_status: Option<u16>,
    /// Reported model id. `None` means `"mock"`, so `Default` keeps the historical value and
    /// no existing test changes.
    model_id: Option<String>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// A backend that always fails, for testing caller-side error handling.
    pub fn failing(message: impl Into<String>) -> Self {
        Self {
            failure: Some(message.into()),
            retryable_status: None,
            model_id: None,
        }
    }

    /// Override the reported model id, so a test with several mocks can prove which answered.
    pub fn with_model_id(mut self, id: impl Into<String>) -> Self {
        self.model_id = Some(id.into());
        self
    }

    /// A backend that always fails with a retryable provider error.
    ///
    /// `failing` produces `InvalidRequest`, which `AiError::is_retryable` correctly reports as
    /// not worth retrying. A caller exercising a fallback path needs the opposite, and a 5xx
    /// `Provider` error is the simplest thing that qualifies — a real `Transport` error would
    /// mean fabricating a `reqwest::Error`, which reqwest gives no way to construct.
    pub fn failing_retryably() -> Self {
        Self {
            failure: None,
            retryable_status: Some(503),
            model_id: None,
        }
    }

    fn check(&self) -> Result<()> {
        if let Some(status) = self.retryable_status {
            return Err(AiError::Provider {
                backend: "mock",
                status,
                message: "the mock backend was asked to fail retryably".into(),
            });
        }
        match &self.failure {
            Some(message) => Err(AiError::InvalidRequest(message.clone())),
            None => Ok(()),
        }
    }
}

#[async_trait]
impl AiBackend for MockBackend {
    fn model_id(&self) -> &str {
        self.model_id.as_deref().unwrap_or("mock")
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn summarize(&self, input: &TranscriptInput) -> Result<SummaryOutput> {
        self.check()?;
        // Echoing the word count keeps the output deterministic while still varying with
        // the input, so tests can assert the transcript actually reached the backend.
        let words = input.text.split_whitespace().count();
        Ok(SummaryOutput {
            text: format!(
                "Mock summary of '{}' ({words} words of transcript).",
                input.title
            ),
            model: self.model_id().to_string(),
        })
    }

    async fn extract_decisions(&self, input: &TranscriptInput) -> Result<Vec<ExtractedDecision>> {
        self.check()?;
        Ok(vec![ExtractedDecision {
            text: format!("Mock decision from '{}'", input.title),
            reasoning: Some("Deterministic output from MockBackend.".into()),
        }])
    }

    async fn extract_action_items(
        &self,
        input: &TranscriptInput,
    ) -> Result<Vec<ExtractedActionItem>> {
        self.check()?;
        Ok(vec![ExtractedActionItem {
            text: format!("Mock action item from '{}'", input.title),
            owner: None,
            due_hint: None,
        }])
    }

    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
        self.check()?;
        if !request.is_valid() {
            return Err(AiError::InvalidRequest(
                "chat requires a non-empty history ending with a user message".into(),
            ));
        }

        let last = request
            .messages
            .last()
            .expect("validity check guarantees a last message");

        Ok(ChatResponse {
            text: format!(
                "Mock reply to '{}' with {} context item(s).",
                last.content,
                request.context.len()
            ),
            model: self.model_id().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChatMessage;

    fn input() -> TranscriptInput {
        TranscriptInput::new("Weekly sync", "We agreed to ship on Friday.")
    }

    #[tokio::test]
    async fn summarize_is_deterministic() {
        let backend = MockBackend::new();
        let first = backend.summarize(&input()).await.unwrap();
        let second = backend.summarize(&input()).await.unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn summary_reflects_the_input() {
        let backend = MockBackend::new();
        let summary = backend.summarize(&input()).await.unwrap();

        assert!(summary.text.contains("Weekly sync"), "{}", summary.text);
        assert!(summary.text.contains("6 words"), "{}", summary.text);
        assert_eq!(summary.model, "mock");
    }

    #[tokio::test]
    async fn mock_reports_itself_as_local() {
        assert!(MockBackend::new().is_local());
    }

    #[tokio::test]
    async fn extraction_returns_results() {
        let backend = MockBackend::new();
        assert_eq!(backend.extract_decisions(&input()).await.unwrap().len(), 1);
        assert_eq!(
            backend.extract_action_items(&input()).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn chat_echoes_the_last_message_and_context_count() {
        let backend = MockBackend::new();
        let request = ChatRequest::new(vec![ChatMessage::user("What did we decide?")])
            .with_context(vec!["transcript".into(), "notes".into()]);

        let response = backend.chat(&request).await.unwrap();
        assert!(response.text.contains("What did we decide?"));
        assert!(response.text.contains("2 context item(s)"));
    }

    #[tokio::test]
    async fn chat_rejects_history_not_ending_with_a_user_message() {
        let backend = MockBackend::new();
        let request = ChatRequest::new(vec![
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
        ]);

        let err = backend
            .chat(&request)
            .await
            .expect_err("should be rejected");
        assert!(matches!(err, AiError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn failing_backend_fails_every_method() {
        let backend = MockBackend::failing("simulated outage");

        assert!(backend.summarize(&input()).await.is_err());
        assert!(backend.extract_decisions(&input()).await.is_err());
        assert!(backend.extract_action_items(&input()).await.is_err());
        assert!(backend
            .chat(&ChatRequest::new(vec![ChatMessage::user("hi")]))
            .await
            .is_err());
    }
}
