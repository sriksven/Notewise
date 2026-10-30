use thiserror::Error;

/// Errors from any backend.
///
/// Deliberately provider-neutral: a caller handling `RateLimited` should not need to know
/// whether it came from Anthropic or a local daemon.
///
/// # The message is for the person using the app
///
/// The Ask panel, a summary that failed and the setup banner all show `to_string()` verbatim.
/// It used to read "could not reach the ollama backend: error sending request for url
/// (http://localhost:11434/api/chat)", which names a URL and not the fix. Every variant now says
/// what happened and what to do about it, in the name the person knows the backend by: "Ollama
/// isn't running. Open the Ollama app…", "Anthropic needs an API key…".
///
/// The technical cause is kept in the fields, and so in the `Debug` form, which is what the
/// server logs record.
#[derive(Debug, Error)]
pub enum AiError {
    #[error(
        "{} needs an API key. Add one in Settings → Models, or switch to Ollama to keep everything on this computer.",
        capitalised(name(.backend))
    )]
    MissingApiKey { backend: &'static str },

    #[error("{}", unreachable(.backend, .source))]
    Transport {
        backend: &'static str,
        #[source]
        source: reqwest::Error,
    },

    #[error("{}", rejected(.backend, *.status, .message))]
    Provider {
        backend: &'static str,
        status: u16,
        message: String,
    },

    #[error(
        "{} is limiting how often Notewise can ask. Try again in {}.",
        capitalised(name(.backend)),
        wait(*.retry_after_secs)
    )]
    RateLimited {
        backend: &'static str,
        retry_after_secs: u64,
    },

    /// The provider returned a well-formed response declining to answer.
    ///
    /// This is a successful HTTP call, not a transport failure — surfaced separately so
    /// callers can show the user something useful instead of "request failed".
    #[error(
        "{} declined to answer this{}.",
        capitalised(name(.backend)),
        .category.as_ref().map(|c| format!(" ({c})")).unwrap_or_default()
    )]
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
    #[error("{}", missing_model(.backend, .model, .installed))]
    ModelNotInstalled {
        backend: &'static str,
        model: String,
        /// Exact tags, as the provider reports them. Empty when the list could not be read.
        installed: Vec<String>,
    },

    #[error(
        "{} sent back an answer Notewise couldn't read. Try again, or pick another model in Settings → Models if it keeps happening.",
        capitalised(name(.backend))
    )]
    MalformedResponse {
        backend: &'static str,
        reason: String,
    },

    /// Shown as written, so a message that can reach a person is written for one.
    #[error("{0}")]
    InvalidRequest(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// What a person calls a backend. The identifiers are the ones the backends report.
fn name(backend: &str) -> &str {
    match backend {
        "ollama" => "Ollama",
        "anthropic" => "Anthropic",
        "gemini" => "Google Gemini",
        "groq" => "Groq",
        "openrouter" => "OpenRouter",
        "lmstudio" => "LM Studio",
        "unsloth" => "Unsloth",
        "openai-compatible" => "your OpenAI-compatible server",
        "mock" => "the mock backend",
        // The email drafter reports itself; what failed is the model behind it.
        "email" => "the model",
        other => other,
    }
}

/// `name` at the start of a sentence: "your OpenAI-compatible server" becomes "Your …".
fn capitalised(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The request never got an answer. What to do depends on whether the backend is a program on
/// this computer, which the person can start, or a service across the internet, which they can
/// only reach.
fn unreachable(backend: &str, source: &reqwest::Error) -> String {
    let who = capitalised(name(backend));

    // Before the timeout check: a connection that timed out is still one that never connected.
    if source.is_connect() {
        return match backend {
            "ollama" => {
                "Ollama isn't running. Open the Ollama app, and Notewise will use it straight away."
                    .into()
            }
            "lmstudio" => "LM Studio's server isn't running. Open LM Studio and start its local server, and Notewise will use it straight away.".into(),
            "unsloth" => {
                "The Unsloth server isn't running. Start it, and Notewise will use it straight away."
                    .into()
            }
            "openai-compatible" => "Couldn't connect to your OpenAI-compatible server. Check that it's running and that its address in Settings → Models is right.".into(),
            _ => format!("Couldn't reach {who}. Check your internet connection."),
        };
    }
    if source.is_timeout() {
        return if matches!(backend, "ollama" | "lmstudio" | "unsloth") {
            format!("{who} took too long to answer. A model can take a minute to load the first time, so try again.")
        } else {
            format!("{who} took too long to answer. Try again.")
        };
    }
    if source.is_decode() || source.is_body() {
        return format!("{who} sent back an answer Notewise couldn't read. Try again.");
    }
    format!("Something went wrong talking to {who}. Try again.")
}

/// The backend answered, with an error status.
fn rejected(backend: &str, status: u16, message: &str) -> String {
    let who = capitalised(name(backend));
    match status {
        401 | 403 => format!("{who} didn't accept the API key. Check it in Settings → Models."),
        404 => format!(
            "{who} doesn't have the model Notewise asked for. Pick another in Settings → Models."
        ),
        // Anthropic's "overloaded" is a 529, so the whole range rather than a list.
        500..=599 => format!("{who} is having trouble right now. Try again in a minute."),
        // Anything else is most often the provider explaining itself in words meant for a
        // person — "Your credit balance is too low…", "prompt is too long" — so it is passed on.
        _ => match message.trim() {
            "" => format!("{who} turned the request down."),
            said => format!("{who} turned the request down: {said}"),
        },
    }
}

/// "30 seconds", "a minute", "5 minutes": how long a rate limit asks for, in words.
fn wait(secs: u64) -> String {
    match secs {
        0 => "a moment".into(),
        1 => "a second".into(),
        2..=59 => format!("{secs} seconds"),
        60 => "a minute".into(),
        _ => format!("{} minutes", secs.div_ceil(60)),
    }
}

/// The model the settings name is not one the backend has.
fn missing_model(backend: &str, model: &str, installed: &[String]) -> String {
    let who = capitalised(name(backend));
    if installed.is_empty() {
        return if backend == "ollama" {
            "Ollama has no model that can chat yet. Download one: in Terminal, run `ollama pull llama3.1`.".into()
        } else {
            format!("{who} has no model that can chat yet.")
        };
    }
    format!(
        "{who} doesn't have “{model}”. Pick one it has from the model menu at the top or in Settings → Models: {}.",
        installed.join(", ")
    )
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

    /// A real failed request, so the test exercises the error reqwest actually produces rather
    /// than one assumed to look like it. Connecting to a port that was just freed is refused.
    async fn refused_connection() -> reqwest::Error {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map(|addr| addr.port())
            .expect("a free port");
        reqwest::get(format!("http://127.0.0.1:{port}/api/chat"))
            .await
            .expect_err("nothing is listening on a port that was just released")
    }

    /// The message this rewrite exists for. It read "could not reach the ollama backend: error
    /// sending request for url (http://localhost:11434/api/chat)" — a URL, not the fix.
    #[tokio::test]
    async fn a_stopped_ollama_says_to_open_it() {
        let err = AiError::Transport {
            backend: "ollama",
            source: refused_connection().await,
        };
        let shown = err.to_string();

        assert!(shown.starts_with("Ollama isn't running."), "{shown}");
        assert!(shown.contains("Open the Ollama app"), "{shown}");
        for jargon in ["http", "url", "backend", "error sending request"] {
            assert!(!shown.contains(jargon), "{jargon:?} in {shown}");
        }
    }

    // A cloud service cannot be started by the person, only reached.
    #[tokio::test]
    async fn an_unreachable_cloud_service_points_at_the_connection() {
        let err = AiError::Transport {
            backend: "anthropic",
            source: refused_connection().await,
        };
        assert_eq!(
            err.to_string(),
            "Couldn't reach Anthropic. Check your internet connection."
        );
    }

    #[tokio::test]
    async fn a_local_server_behind_the_compatible_backend_is_named() {
        let err = AiError::Transport {
            backend: "lmstudio",
            source: refused_connection().await,
        };
        assert!(
            err.to_string()
                .contains("Open LM Studio and start its local server"),
            "{err}"
        );
    }

    // A model being loaded into memory is the usual reason a local backend is slow to answer,
    // and the request after it is fast. The listener accepts the connection and never replies.
    #[tokio::test]
    async fn a_slow_local_model_says_to_try_again() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        let port = listener.local_addr().expect("an address").port();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(200))
            .build()
            .expect("a client");
        let source = client
            .get(format!("http://127.0.0.1:{port}/api/chat"))
            .send()
            .await
            .expect_err("the listener never answers");
        drop(listener);

        let shown = AiError::Transport {
            backend: "ollama",
            source,
        }
        .to_string();
        assert!(
            shown.starts_with("Ollama took too long to answer."),
            "{shown}"
        );
        assert!(shown.contains("try again"), "{shown}");
    }

    #[test]
    fn a_missing_key_says_where_to_add_one() {
        let shown = AiError::MissingApiKey {
            backend: "anthropic",
        }
        .to_string();
        assert!(shown.starts_with("Anthropic needs an API key."), "{shown}");
        assert!(shown.contains("Settings → Models"), "{shown}");
    }

    #[test]
    fn a_rejected_key_is_named_as_the_key() {
        for status in [401, 403] {
            let shown = AiError::Provider {
                backend: "gemini",
                status,
                message: "API_KEY_INVALID".into(),
            }
            .to_string();
            assert_eq!(
                shown,
                "Google Gemini didn't accept the API key. Check it in Settings → Models."
            );
        }
    }

    #[test]
    fn a_provider_outage_says_to_wait() {
        // 529 is Anthropic's "overloaded", outside the usual 5xx a list would name.
        for status in [500, 503, 529] {
            let shown = AiError::Provider {
                backend: "anthropic",
                status,
                message: "overloaded_error".into(),
            }
            .to_string();
            assert!(
                shown.contains("having trouble right now"),
                "{status}: {shown}"
            );
            assert!(!shown.contains("overloaded_error"), "{shown}");
        }
    }

    // The provider's own words are passed on when they are the explanation.
    #[test]
    fn a_turned_down_request_keeps_the_providers_reason() {
        let shown = AiError::Provider {
            backend: "anthropic",
            status: 400,
            message: "Your credit balance is too low.".into(),
        }
        .to_string();
        assert_eq!(
            shown,
            "Anthropic turned the request down: Your credit balance is too low."
        );
    }

    #[test]
    fn a_rate_limit_says_how_long_in_words() {
        let after = |retry_after_secs| {
            AiError::RateLimited {
                backend: "groq",
                retry_after_secs,
            }
            .to_string()
        };
        assert!(after(0).ends_with("Try again in a moment."), "{}", after(0));
        assert!(
            after(30).ends_with("Try again in 30 seconds."),
            "{}",
            after(30)
        );
        assert!(
            after(60).ends_with("Try again in a minute."),
            "{}",
            after(60)
        );
        assert!(
            after(150).ends_with("Try again in 3 minutes."),
            "{}",
            after(150)
        );
    }

    /// Every identifier a backend reports has a name for people. One that fell through would
    /// put "openai-compatible" or "lmstudio" in front of someone.
    #[test]
    fn every_backend_is_named_for_people() {
        for (id, expected) in [
            ("ollama", "Ollama"),
            ("anthropic", "Anthropic"),
            ("gemini", "Google Gemini"),
            ("groq", "Groq"),
            ("openrouter", "OpenRouter"),
            ("lmstudio", "LM Studio"),
            ("unsloth", "Unsloth"),
            ("openai-compatible", "Your OpenAI-compatible server"),
        ] {
            let shown = AiError::MissingApiKey { backend: id }.to_string();
            assert!(shown.starts_with(expected), "{id}: {shown}");
        }
    }
}
