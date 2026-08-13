//! Speech-to-text.
//!
//! Consumes audio frames and emits timestamped segments. Whisper.cpp and Parakeet sit behind
//! one interface, so swapping engines is configuration rather than a rewrite.
//!
//! # Features
//!
//! Whisper inference is **implemented and working**, behind the `whisper` feature. It is off
//! by default because it pulls in a cmake build of whisper.cpp and needs a 150 MB–1.5 GB
//! model download — neither belongs in a default `cargo build`.
//!
//! | Feature | Effect |
//! |---|---|
//! | `whisper` | whisper.cpp inference on CPU |
//! | `whisper-metal` | GPU on Apple silicon |
//! | `whisper-cuda` | GPU on NVIDIA |
//! | `whisper-vulkan` | GPU on AMD/Intel |
//!
//! Measured on an Apple M4 with `base.en`: **37.6x realtime on Metal, 25.2x on CPU**.
//!
//! [`MockEngine`] is real and deterministic, which is what lets the pipeline above be tested
//! with no model and no C++ toolchain.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod engine;
mod models;
mod segment;

pub use engine::{MockEngine, TranscriptionEngine, WhisperEngine};
// Re-exported so downstream crates can implement the trait without adding the dependency.
pub use async_trait::async_trait;
pub use models::{DownloadProgress, ModelInfo, ModelRegistry, ModelSize, ModelStore};
pub use segment::{Segment, Transcript};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TranscriptionError {
    #[error("{engine} inference is not available: {reason}")]
    EngineUnavailable {
        engine: &'static str,
        reason: &'static str,
    },

    #[error("model '{0}' is not in the registry")]
    UnknownModel(String),

    #[error("model '{name}' is not downloaded; run the model download step first")]
    ModelNotDownloaded { name: String },

    #[error("model '{name}' failed its integrity check: expected {expected} bytes, got {actual}")]
    ModelCorrupt {
        name: String,
        expected: u64,
        actual: u64,
    },

    #[error("audio is not in the expected format: {0}")]
    BadAudio(String),

    #[error("download failed: {0}")]
    Download(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, TranscriptionError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unavailable_engine_says_why() {
        let err = TranscriptionError::EngineUnavailable {
            engine: "whisper",
            reason: "built without the 'whisper' feature",
        };
        assert!(err.to_string().contains("feature"), "{err}");
    }

    #[test]
    fn a_corrupt_model_error_reports_both_sizes() {
        let err = TranscriptionError::ModelCorrupt {
            name: "base.en".into(),
            expected: 148_000_000,
            actual: 1024,
        };
        let message = err.to_string();
        assert!(message.contains("148000000"), "{message}");
        assert!(message.contains("1024"), "{message}");
    }
}
