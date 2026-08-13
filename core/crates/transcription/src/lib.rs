//! Speech-to-text.
//!
//! Consumes audio frames and emits timestamped segments. Whisper.cpp and Parakeet sit behind
//! one interface, so swapping engines is configuration rather than a rewrite.
//!
//! # What is and is not implemented here
//!
//! The engine interface, segment types, the model registry, and the model store (path
//! resolution, download, integrity verification) are complete and tested. **Whisper inference
//! itself is behind the `whisper` feature and not implemented** — it needs a cmake build of
//! whisper.cpp and a 150 MB–1.5 GB model download, neither of which belongs in a default
//! `cargo build`. [`MockEngine`] is real and produces deterministic output, which is what
//! lets the pipeline above be tested without any of that.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod engine;
mod models;
mod segment;

pub use engine::{MockEngine, TranscriptionEngine, WhisperEngine};
pub use models::{ModelInfo, ModelRegistry, ModelSize, ModelStore};
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
