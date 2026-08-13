//! The AI router — the seam the local-or-cloud promise rests on.
//!
//! Every feature that needs a model calls [`AiBackend`]. Nothing above this crate imports a
//! provider SDK or hits a provider URL. That is what makes "local or cloud, your choice" a
//! property the compiler enforces rather than a claim in a README.
//!
//! # Backends
//!
//! | Backend | Runs | Needs |
//! |---|---|---|
//! | [`MockBackend`] | In-process | Nothing — exists so the seam stays testable |
//! | [`OllamaBackend`] | Local machine | A running Ollama daemon |
//! | [`AnthropicBackend`] | Anthropic API | The user's own API key (BYOK) |
//! | [`GeminiBackend`] | Google Gemini | The user's own API key |
//! | [`OpenAiCompatBackend`] | Groq, OpenRouter, LM Studio, Unsloth, or any custom endpoint | A key, unless the endpoint is local |
//!
//! `MockBackend` is not a test fixture that leaked into the public API. A boundary is only
//! protected if it is testable; without a mock, every test touching summarization would need
//! a GPU or a paid API key, so those tests get skipped and the seam quietly erodes.
//!
//! # Example
//!
//! ```
//! use notewise_ai_router::{AiBackend, MockBackend, TranscriptInput};
//!
//! # async fn example() -> Result<(), notewise_ai_router::AiError> {
//! let backend = MockBackend::new();
//! let summary = backend
//!     .summarize(&TranscriptInput::new("Standup", "We agreed to ship Friday."))
//!     .await?;
//!
//! assert_eq!(summary.model, "mock");
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod backends;
mod error;
mod router;
mod types;

pub use backends::{
    AnthropicBackend, GeminiBackend, MockBackend, OllamaBackend, OpenAiCompatBackend, Preset,
};
pub use error::{AiError, Result};
pub use router::{BackendKind, Router, RouterConfig};
pub use types::{
    ChatMessage, ChatRequest, ChatResponse, ExtractedActionItem, ExtractedDecision, Role,
    SummaryOutput, TranscriptInput,
};

use async_trait::async_trait;

/// The one interface every model provider hides behind.
///
/// Implementors must be `Send + Sync` so a single backend can be shared across the async
/// runtime — the API server holds one and serves concurrent requests from it.
#[async_trait]
pub trait AiBackend: Send + Sync + std::fmt::Debug {
    /// Human-readable identifier for the model actually in use, recorded alongside
    /// generated output so it can be audited or regenerated later.
    fn model_id(&self) -> &str;

    /// Whether this backend keeps data on the user's machine.
    ///
    /// Surfaced in the UI so "local only" is something a user can verify rather than trust.
    fn is_local(&self) -> bool;

    async fn summarize(&self, input: &TranscriptInput) -> Result<SummaryOutput>;

    async fn extract_decisions(&self, input: &TranscriptInput) -> Result<Vec<ExtractedDecision>>;

    async fn extract_action_items(
        &self,
        input: &TranscriptInput,
    ) -> Result<Vec<ExtractedActionItem>>;

    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trait must stay object-safe — the router stores `Box<dyn AiBackend>`, and losing
    /// object safety would break the whole design.
    #[test]
    fn backend_trait_is_object_safe() {
        let _boxed: Box<dyn AiBackend> = Box::new(MockBackend::new());
    }

    #[tokio::test]
    async fn backends_are_usable_behind_a_trait_object() {
        let backend: Box<dyn AiBackend> = Box::new(MockBackend::new());
        let input = TranscriptInput::new("Standup", "We agreed to ship Friday.");

        assert!(!backend.summarize(&input).await.unwrap().text.is_empty());
        assert!(backend.is_local());
    }
}
