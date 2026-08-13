//! Backend implementations.
//!
//! Adding a backend means implementing [`AiBackend`](crate::AiBackend) here and adding a
//! [`BackendKind`](crate::BackendKind) variant. Nothing outside this module needs to change —
//! that is the property the seam exists to provide.

mod anthropic;
mod mock;
mod ollama;

pub use anthropic::AnthropicBackend;
pub use mock::MockBackend;
pub use ollama::OllamaBackend;
