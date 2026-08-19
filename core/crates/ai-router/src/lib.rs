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
mod clarify;
mod email;
mod embed;
mod policy;
mod error;
mod redact;
mod router;
mod types;

pub use backends::{
    AnthropicBackend, GeminiBackend, MockBackend, OllamaBackend, OpenAiCompatBackend, Preset,
};
pub use clarify::{
    parse_questions, suggest_questions, AmbiguityKind, ClarifierConfig, ClarifierSession,
    ClarifyingQuestion, Utterance,
};
pub use email::{
    generate_email_draft, generate_email_variants, EmailContext, EmailTone, GeneratedEmail,
};
pub use embed::{cosine, is_embedding_model, Embedder, DEFAULT_MODEL as DEFAULT_EMBEDDING_MODEL};
pub use error::{AiError, Result};
pub use redact::{redact, Category, RedactionPolicy, RedactionReport};
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

    /// Whether this backend is usable right now.
    ///
    /// The default answers yes without a network call, which is correct for every hosted
    /// provider: the backend was constructed, so a key was present, and spending a real
    /// completion to prove the endpoint is up would cost money on every launch. Local
    /// backends that depend on a separate daemon override this.
    async fn probe(&self) -> Result<()> {
        Ok(())
    }

    /// The model names this backend can actually run, if it can be asked.
    ///
    /// Empty by default. A local daemon holds whatever the user has pulled, and the names are
    /// exact tags — `llama3.1:8b` is not `llama3.1`, and a UI that offers the second when only
    /// the first is installed sends the user into a 404 with no way to correct it. Hosted
    /// providers return empty: their catalogues change without us, and listing them costs a
    /// round trip against a metered key.
    async fn installed_models(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
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

    /// Cloud backends inherit the default: having been constructed, a key was present, and
    /// that is the check. Spending a real completion on every launch to prove the endpoint is
    /// up would cost money for an answer nobody reads.
    #[tokio::test]
    async fn the_default_probe_succeeds_without_a_network_call() {
        let backend: Box<dyn AiBackend> = Box::new(MockBackend::new());
        assert!(backend.probe().await.is_ok());
    }
}
