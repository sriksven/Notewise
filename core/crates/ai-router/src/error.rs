use thiserror::Error;

/// Errors from any backend.
///
/// Deliberately provider-neutral: a caller handling `RateLimited` should not need to know
/// whether it came from Anthropic or a local daemon.
#[derive(Debug, Error)]
pub enum AiError {
    #[error("no API key configured for backend '{backend}'")]
    MissingApiKey { backend: &'static str },

    #[error("could not reach the {backend} backend: {source}")]
    Transport {
        backend: &'static str,
        #[source]
        source: reqwest::Error,
    },

    #[error("{backend} rejected the request ({status}): {message}")]
    Provider {
        backend: &'static str,
        status: u16,
        message: String,
    },

    #[error("{backend} is rate limited; retry after {retry_after_secs}s")]
    RateLimited {
        backend: &'static str,
        retry_after_secs: u64,
    },

    /// The provider returned a well-formed response declining to answer.
    ///
    /// This is a successful HTTP call, not a transport failure — surfaced separately so
    /// callers can show the user something useful instead of "request failed".
    #[error("{backend} declined to answer{}", .category.as_ref().map(|c| format!(" ({c})")).unwrap_or_default())]
    Refused {
        backend: &'static str,
        category: Option<String>,
    },

    /// The configured model is not present on the provider.
    ///
    /// Separate from [`AiError::Provider`] because the raw provider message — "model
    /// 'llama3.1' not found" — names the problem and not the fix, and the fix is a choice only
    /// this error can offer: the backend is the one thing that knows what *is* installed.
    ///
    /// The trap is specific to tags. Ollama resolves a bare `llama3.1` to `llama3.1:latest`,
    /// so a machine holding `llama3.1:8b` genuinely has no such model, and a user staring at
    /// "not found" has no way to discover that from the message alone.
    #[error("{backend} has no model named {model:?}. Installed: {}", if .installed.is_empty() { "nothing that can hold a conversation. Pull one, e.g. `ollama pull llama3.1`".to_string() } else { format!("{}. Pick one in Settings", .installed.join(", ")) })]
    ModelNotInstalled {
        backend: &'static str,
        model: String,
        /// Exact tags, as the provider reports them. Empty when the list could not be read.
        installed: Vec<String>,
    },

    #[error("could not parse the {backend} response: {reason}")]
    MalformedResponse {
        backend: &'static str,
        reason: String,
    },

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, AiError>;

impl AiError {
    /// Whether retrying the identical request could plausibly succeed.
    ///
    /// A refusal is deliberately **not** retryable — the same input produces the same
    /// refusal, so retrying just burns quota.
    pub fn is_retryable(&self) -> bool {
        match self {
            AiError::Transport { .. } | AiError::RateLimited { .. } => true,
            AiError::Provider { status, .. } => *status >= 500,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_model_is_not_retryable() {
        // Retrying sends the same absent model name to the same daemon. The fix is a choice,
        // not a second attempt.
        let err = AiError::ModelNotInstalled {
            backend: "ollama",
            model: "llama3.1".into(),
            installed: vec!["llama3.1:8b".into()],
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn server_errors_are_retryable_client_errors_are_not() {
        let server = AiError::Provider {
            backend: "anthropic",
            status: 503,
            message: "overloaded".into(),
        };
        let client = AiError::Provider {
            backend: "anthropic",
            status: 400,
            message: "bad request".into(),
        };

        assert!(server.is_retryable());
        assert!(!client.is_retryable());
    }

    #[test]
    fn rate_limits_are_retryable() {
        assert!(AiError::RateLimited {
            backend: "anthropic",
            retry_after_secs: 30,
        }
        .is_retryable());
    }

    #[test]
    fn refusals_are_not_retryable() {
        let err = AiError::Refused {
            backend: "anthropic",
            category: Some("cyber".into()),
        };
        assert!(
            !err.is_retryable(),
            "retrying a refusal produces the same refusal and wastes quota"
        );
    }

    #[test]
    fn refusal_message_includes_category_when_known() {
        let with = AiError::Refused {
            backend: "anthropic",
            category: Some("cyber".into()),
        };
        let without = AiError::Refused {
            backend: "anthropic",
            category: None,
        };

        assert!(with.to_string().contains("(cyber)"), "{with}");
        assert!(!without.to_string().contains('('), "{without}");
    }

    #[test]
    fn missing_key_is_not_retryable() {
        assert!(!AiError::MissingApiKey {
            backend: "anthropic"
        }
        .is_retryable());
    }
}
