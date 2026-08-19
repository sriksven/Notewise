//! Turning text into vectors, locally.
//!
//! # Why this is not an `AiBackend` method
//!
//! The obvious design is `AiBackend::embed`, so whichever provider is answering questions also
//! produces the vectors. It is the wrong design here, for one reason: embedding a workspace
//! means sending **every meeting, every note, and every ticket** to whatever is on the other
//! end. A user who picked Anthropic to summarize one meeting has consented to that meeting
//! leaving their machine. They have not consented to their entire history leaving it, in the
//! background, because a search index needed building.
//!
//! So embedding is its own thing and it is local-only. There is no hosted embedder and no
//! configuration that would produce one. If Ollama is not running, the workspace is not
//! embedded and search stays lexical — a degradation, not a prompt to send data somewhere.
//!
//! # Why the model name travels with every vector
//!
//! Cosine distance between vectors from two different models is not a small error, it is
//! meaningless — and the bytes do not say which model produced them. Without recording it, a
//! user switching from `nomic-embed-text` to `bge-m3` gets confident nonsense rather than an
//! obvious failure. [`Embedder::model`] is stored alongside every vector and checked before
//! anything is compared.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{AiError, Result};
use crate::tags;

const BACKEND: &str = "ollama";

/// Ollama's batch embedding endpoint. `/api/embeddings` is the older single-input form.
const DEFAULT_ENDPOINT: &str = "http://localhost:11434/api/embed";

/// The default embedding model.
///
/// `nomic-embed-text` rather than something larger: it is 274 MB, produces 768 dimensions, and
/// is the model most people running Ollama already have. A better default that nobody has
/// pulled is not a default, it is a failure with an extra step.
///
/// Untagged, and therefore a *family* rather than a model. Ollama expands a bare name to
/// `:latest`, so sending this string unchanged asserts the user pulled that exact tag — true
/// on most machines and false on any that pulled a pinned version. [`Embedder::available`]
/// already answered the question by family; the request did not, so the two could disagree
/// about the same daemon. Resolution happens in [`Embedder::wire_model`].
pub const DEFAULT_MODEL: &str = "nomic-embed-text";

/// Models known to produce embeddings rather than conversation.
///
/// Used to filter what the daemon reports so a settings screen does not offer `llama3.1` as an
/// embedder. Matched as a prefix, because tags carry a suffix (`bge-m3:latest`).
const KNOWN_EMBEDDERS: &[&str] = &[
    "nomic-embed-text",
    "bge-m3",
    "bge-large",
    "bge-small",
    "bge-base",
    "mxbai-embed-large",
    "all-minilm",
    "snowflake-arctic-embed",
    "paraphrase-multilingual",
    "granite-embedding",
];

/// Whether a model tag names an embedding model.
pub fn is_embedding_model(tag: &str) -> bool {
    let name = tag.split(':').next().unwrap_or(tag).to_lowercase();
    KNOWN_EMBEDDERS.iter().any(|known| name.starts_with(known))
}

/// The task prefixes a model expects, as `(query, document)`.
///
/// Several embedding models are trained with an instruction prefix that tells them whether the
/// text is a search query or a document being indexed, and they are *asymmetric* — the two get
/// different prefixes on purpose, because a question and the passage answering it do not look
/// alike. Omitting them does not fail; it just uses the model off-distribution and returns
/// worse-calibrated similarities.
///
/// Empty for models that were not trained this way. Adding a prefix a model does not expect is
/// as wrong as omitting one it does.
fn prefixes(model: &str) -> (&'static str, &'static str) {
    let name = model.split(':').next().unwrap_or(model).to_lowercase();

    if name.starts_with("nomic-embed-text") {
        ("search_query: ", "search_document: ")
    } else {
        // bge-m3 and the E5-style models in this list are trained symmetric, or their
        // instruction is optional and helps only on specific benchmarks.
        ("", "")
    }
}

/// A local embedding model, reached through Ollama.
#[derive(Debug, Clone)]
pub struct Embedder {
    endpoint: String,
    /// The model asked for, which may name a family rather than a tag.
    model: String,
    /// The tag actually sent, resolved once against the daemon's model list.
    resolved: Arc<tokio::sync::OnceCell<String>>,
    http: reqwest::Client,
}

impl Embedder {
    /// An embedder that has **not** resolved its tag.
    ///
    /// [`Self::model`] returns the name as given, which is only exact if the caller already passed
    /// a tag. Prefer [`Self::connect`] anywhere the label will be stored: two embedders
    /// disagreeing about what to call the same model is what mixes vectors.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            model: model.into(),
            resolved: Arc::new(tokio::sync::OnceCell::new()),
            http: reqwest::Client::new(),
        }
    }

    /// An embedder whose tag is resolved **now**, so its label is exact and never changes.
    ///
    /// # Why this is async, and why that is the whole point
    ///
    /// Resolution has to ask the daemon. Doing it lazily — on the first embed — means
    /// [`Self::model`] returns one thing before a request and another after, and since that value
    /// both labels stored vectors *and* decides which chunks are still pending, the label written
    /// would stop matching the label queried and every run would re-embed the workspace.
    ///
    /// Resolving at construction removes the flip-flop instead of working around it: the value is
    /// fixed before anything can read it, and stays fixed for this embedder's lifetime.
    ///
    /// If the daemon cannot be reached the requested name is kept, which is stable too — and
    /// embedding needs the daemon anyway, so nothing gets stored under the wrong label.
    pub async fn connect(model: impl Into<String>) -> Self {
        Self::new(model).resolved().await
    }

    /// Resolve this embedder's tag against the daemon it points at.
    ///
    /// A method rather than a second constructor so it composes with [`Self::with_endpoint`] —
    /// `Embedder::new(m).with_endpoint(url).resolved().await` resolves against *that* daemon.
    /// A `connect(model, endpoint)` constructor would have had to grow a parameter for every
    /// builder method that already exists.
    pub async fn resolved(self) -> Self {
        let model = self.wire_model().await;
        Self { model, ..self }
    }

    /// Point at a daemon somewhere other than localhost.
    ///
    /// Takes the `/api/embed` URL, matching how the chat backend is configured, so the two
    /// cannot disagree about what "the endpoint" means.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Which model produces these vectors. Recorded with every one that is stored.
    ///
    /// Exact when the embedder came from [`Self::connect`], which is what production uses; the
    /// name as given when it came from [`Self::new`].
    ///
    /// Either way it is **fixed for this embedder's lifetime**. That is the property that matters:
    /// this value labels stored vectors *and* decides which chunks are still pending, so a value
    /// that changed part-way through a process would make every run believe the whole workspace
    /// was pending and re-embed it forever.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The tag to actually send, resolved once against what the daemon holds.
    ///
    /// Only ever within the same family. Substituting a different embedder would be the one
    /// mistake this module exists to prevent: vectors from two models are not slightly
    /// inconsistent, they are incomparable, and the label stored beside them would say
    /// otherwise. If the family is absent the requested name goes out unchanged and the
    /// daemon's own 404 is the answer.
    async fn wire_model(&self) -> String {
        self.resolved
            .get_or_init(|| async {
                let Ok(installed) = self.installed().await else {
                    return self.model.clone();
                };
                tags::resolve_tag(&self.model, &installed).unwrap_or_else(|| self.model.clone())
            })
            .await
            .clone()
    }

    fn base(&self) -> &str {
        self.endpoint
            .strip_suffix("/api/embed")
            .unwrap_or(&self.endpoint)
    }

    /// Embed material being indexed.
    ///
    /// Batched rather than one call per chunk: a workspace is thousands of chunks and the
    /// per-request overhead dominates at that size. Ollama holds the model in memory across
    /// the batch, so this is also where most of the speed comes from.
    ///
    /// Returns vectors in the same order as `texts`, and errors if the daemon returns a
    /// different number than were asked for — a mismatched batch would silently attach each
    /// vector to the wrong chunk, which is worse than failing.
    pub async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let (_, prefix) = prefixes(&self.model);
        self.embed_with(prefix, texts).await
    }

    /// Embed a question.
    ///
    /// Deliberately separate from [`Self::embed_documents`]: the two get different task
    /// prefixes, and using the document form for a query is the most common way to get quietly
    /// worse retrieval from a model that supports them.
    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let (prefix, _) = prefixes(&self.model);
        let mut vectors = self.embed_with(prefix, &[text.to_string()]).await?;
        vectors.pop().ok_or_else(|| AiError::MalformedResponse {
            backend: BACKEND,
            reason: "no embedding for the query".into(),
        })
    }

    async fn embed_with(&self, prefix: &str, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let prefixed: Vec<String> = if prefix.is_empty() {
            texts.to_vec()
        } else {
            texts.iter().map(|text| format!("{prefix}{text}")).collect()
        };
        let texts = &prefixed;

        let response = self
            .http
            .post(&self.endpoint)
            .json(&EmbedRequest {
                model: &self.wire_model().await,
                input: texts,
            })
            // Generous: a cold model has to be loaded from disk first, and a large batch of
            // long chunks is real work. Still bounded, so a wedged daemon does not hang an
            // indexing run forever.
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
            .map_err(|source| AiError::Transport {
                backend: BACKEND,
                source,
            })?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(AiError::Provider {
                backend: BACKEND,
                status: status.as_u16(),
                message,
            });
        }

        let body: EmbedResponse =
            response
                .json()
                .await
                .map_err(|source| AiError::MalformedResponse {
                    backend: BACKEND,
                    reason: source.to_string(),
                })?;

        if body.embeddings.len() != texts.len() {
            return Err(AiError::MalformedResponse {
                backend: BACKEND,
                reason: format!(
                    "asked for {} embeddings and got {}",
                    texts.len(),
                    body.embeddings.len()
                ),
            });
        }

        // A zero-width vector would sail through every later check and make every cosine
        // similarity NaN. Reject it here, where the cause is still visible.
        if let Some(empty) = body.embeddings.iter().position(Vec::is_empty) {
            return Err(AiError::MalformedResponse {
                backend: BACKEND,
                reason: format!("embedding {empty} came back empty"),
            });
        }

        Ok(body.embeddings)
    }

    /// Whether the daemon is up and holds this model.
    ///
    /// Cheap: `/api/tags` loads nothing. Used to decide whether semantic search is available
    /// at all, so it must answer quickly rather than block a settings screen.
    pub async fn available(&self) -> bool {
        self.installed().await.is_ok_and(|models| {
            models
                .iter()
                .any(|tag| tag == &self.model || tag.starts_with(&format!("{}:", self.model)))
        })
    }

    /// Embedding models this daemon holds.
    pub async fn installed(&self) -> Result<Vec<String>> {
        let response = self
            .http
            .get(format!("{}/api/tags", self.base()))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .map_err(|source| AiError::Transport {
                backend: BACKEND,
                source,
            })?;

        if !response.status().is_success() {
            return Err(AiError::Provider {
                backend: BACKEND,
                status: response.status().as_u16(),
                message: "could not list models".into(),
            });
        }

        let body: TagsResponse =
            response
                .json()
                .await
                .map_err(|source| AiError::MalformedResponse {
                    backend: BACKEND,
                    reason: source.to_string(),
                })?;

        Ok(body
            .models
            .into_iter()
            .map(|model| model.name)
            .filter(|name| is_embedding_model(name))
            .collect())
    }
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagEntry>,
}

#[derive(Deserialize)]
struct TagEntry {
    name: String,
}

/// Cosine similarity, in `[-1, 1]`.
///
/// Returns 0 for a length mismatch or a zero vector rather than `NaN`. Both are real
/// possibilities — the first when vectors from two models are compared despite the model
/// check, the second from a degenerate embedding — and a `NaN` propagates silently through a
/// sort into an arbitrary ranking, while a 0 simply ranks last.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let denominator = norm_a.sqrt() * norm_b.sqrt();
    if denominator == 0.0 {
        return 0.0;
    }

    dot / denominator
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards an infinite re-index. The label must not change part-way through a process: it
    /// names stored vectors *and* decides which chunks are pending, so a value that moved would
    /// make the label written stop matching the label queried, forever.
    ///
    /// The property is *stability*, not which name is used — which is why resolving eagerly in
    /// `connect` is safe and resolving lazily was not.
    #[test]
    fn an_unresolved_embedder_keeps_the_name_it_was_given() {
        let embedder = Embedder::new("nomic-embed-text");
        assert_eq!(embedder.model(), "nomic-embed-text");
        assert_eq!(
            embedder.clone().model(),
            "nomic-embed-text",
            "a clone must agree, since indexing builds a fresh embedder per pass"
        );
    }

    #[tokio::test]
    async fn connecting_to_an_unreachable_daemon_keeps_the_requested_name_and_stays_fixed() {
        // Port 1 is reserved and nothing listens there, so resolution cannot succeed. The endpoint
        // has to be set *before* resolving, which is why `resolved` is a method and not a second
        // constructor.
        let embedder = Embedder::new("nomic-embed-text")
            .with_endpoint("http://127.0.0.1:1/api/embed")
            .resolved()
            .await;

        let first = embedder.model().to_string();
        assert_eq!(first, "nomic-embed-text");
        assert_eq!(
            embedder.model(),
            first,
            "the label must not move once anything could have read it"
        );
    }

    /// The residual this change closes: a stored label that names a family rather than the tag
    /// that actually produced the vectors, so pulling a second tag of the same family could mix
    /// incomparable vectors under one name.
    ///
    /// `#[ignore]`d because it needs a running Ollama with an embedding model pulled. Run with
    /// `cargo test -p notewise-ai-router -- --ignored connect_resolves`.
    #[tokio::test]
    #[ignore = "needs a running Ollama daemon with an embedding model pulled"]
    async fn connect_resolves_the_label_to_an_exact_tag() {
        let embedder = Embedder::connect("nomic-embed-text").await;
        assert!(
            embedder.model().contains(':'),
            "expected an exact tag, got {:?}",
            embedder.model()
        );
    }

    #[test]
    fn identical_vectors_are_maximally_similar() {
        let v = vec![0.1, 0.2, 0.3];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn orthogonal_vectors_score_zero() {
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn opposite_vectors_score_minus_one() {
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
    }

    /// The failure this prevents: a `NaN` sorts unpredictably and silently scrambles a
    /// ranking, where a zero simply comes last.
    #[test]
    fn degenerate_inputs_score_zero_rather_than_nan() {
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0, "length mismatch");
    }

    #[test]
    fn magnitude_does_not_change_direction() {
        // Cosine is scale-invariant, which is why vectors do not need normalizing on the way in.
        let a = [1.0, 2.0, 3.0];
        let scaled = [10.0, 20.0, 30.0];
        assert!((cosine(&a, &scaled) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn embedding_models_are_recognised_with_and_without_a_tag() {
        assert!(is_embedding_model("nomic-embed-text"));
        assert!(is_embedding_model("nomic-embed-text:latest"));
        assert!(is_embedding_model("bge-m3:567m"));
        assert!(is_embedding_model("mxbai-embed-large"));
    }

    #[test]
    fn chat_models_are_not_offered_as_embedders() {
        assert!(!is_embedding_model("llama3.1:8b"));
        assert!(!is_embedding_model("llama3:latest"));
        assert!(!is_embedding_model("qwen2.5-coder"));
        assert!(!is_embedding_model(""));
    }

    /// Getting these backwards is silent — retrieval simply gets worse — so they are asserted
    /// rather than trusted.
    #[test]
    fn nomic_gets_its_asymmetric_task_prefixes() {
        let (query, document) = prefixes("nomic-embed-text");
        assert_eq!(query, "search_query: ");
        assert_eq!(document, "search_document: ");
        assert_ne!(query, document, "the two must not be the same");

        // Tags carry a suffix.
        assert_eq!(prefixes("nomic-embed-text:latest").0, "search_query: ");
        assert_eq!(prefixes("nomic-embed-text:v1.5").0, "search_query: ");
    }

    /// Adding a prefix a model was not trained with is as wrong as omitting one it was.
    #[test]
    fn models_without_task_prefixes_get_none() {
        for model in ["bge-m3", "bge-m3:567m", "mxbai-embed-large", "all-minilm"] {
            assert_eq!(prefixes(model), ("", ""), "{model} should get no prefix");
        }
    }

    #[test]
    fn the_base_url_is_recovered_from_the_endpoint() {
        let embedder = Embedder::new("x");
        assert_eq!(embedder.base(), "http://localhost:11434");

        let remote = Embedder::new("x").with_endpoint("http://192.168.1.10:11434/api/embed");
        assert_eq!(remote.base(), "http://192.168.1.10:11434");
    }

    #[tokio::test]
    async fn an_empty_batch_makes_no_request() {
        // Pointed at a closed port: if this tried to connect it would error rather than
        // return an empty vector.
        let embedder = Embedder::new("x").with_endpoint("http://127.0.0.1:1/api/embed");
        assert_eq!(
            embedder.embed_documents(&[]).await.unwrap(),
            Vec::<Vec<f32>>::new()
        );
    }

    #[tokio::test]
    async fn an_unreachable_daemon_is_not_available_rather_than_an_error() {
        let embedder =
            Embedder::new("nomic-embed-text").with_endpoint("http://127.0.0.1:1/api/embed");
        assert!(!embedder.available().await);
    }
}
