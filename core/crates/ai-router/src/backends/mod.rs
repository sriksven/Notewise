//! Backend implementations.
//!
//! Adding a backend means implementing [`AiBackend`](crate::AiBackend) here and adding a
//! [`BackendKind`](crate::BackendKind) variant. Nothing outside this module needs to change —
//! that is the property the seam exists to provide.

mod anthropic;
mod gemini;
mod mock;
mod ollama;
mod openai_compat;

pub use anthropic::AnthropicBackend;
pub use gemini::GeminiBackend;
pub use mock::MockBackend;
pub use ollama::OllamaBackend;
pub use openai_compat::{OpenAiCompatBackend, Preset};
