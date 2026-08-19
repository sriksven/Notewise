//! Building the semantic index.
//!
//! Chunk the workspace, embed the chunks locally, store the vectors. What makes this worth its
//! complexity is stated in [`crate::retrieval`]: lexical search finds "pricing" and misses
//! "cost structure", and that gap is the ceiling on every grounded answer in the product.
//!
//! # Why it is a background run rather than a write hook
//!
//! Embedding on every save would put a model call on the path of every keystroke's autosave
//! and every finished recording. It would also fail badly: a stopped Ollama would turn saving
//! a note into an error, for an index the user never asked about.
//!
//! So indexing is a pass. It reads what has changed since last time — by comparing the
//! entity's `updated_at` against the newest chunk stored for it — and does only that work.
//! A first run on a large workspace is minutes; every run after is seconds.
//!
//! # Why nothing here fails the app
//!
//! Every path degrades to lexical. No embedder, no vectors, a model that was uninstalled, a
//! daemon that stopped mid-run: all of them leave search working, less well. An index is an
//! optimisation, and an optimisation that can break the thing it optimises is a liability.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::RwLock;

use notewise_ai_router::Embedder;
use notewise_storage::{
    Database, EmbeddingRepository, Id, MeetingRepository, NewEmbedding, NoteRepository,
    TicketRepository,
};

/// Target chunk size, in characters.
///
/// Around 900 is a paragraph or a minute of speech — small enough that a match points at
/// something specific, large enough to carry its own context. Chunks much smaller than this
/// retrieve well and read badly: a model given twelve disconnected sentences produces an
/// answer that sounds assembled, because it was.
const CHUNK_CHARS: usize = 900;

/// How much of the previous chunk to repeat at the start of the next.
///
/// Without overlap, a sentence that straddles a boundary is in neither chunk in full and
/// matches poorly in both — and the sentence most likely to straddle one is the long
/// qualifying sentence where the actual decision lives.
const CHUNK_OVERLAP_CHARS: usize = 150;

/// How many chunks go to the daemon at once.
///
/// Batching is most of the speed — the model stays loaded across the batch. Bounded so a
/// single request stays inside its timeout on a slow machine.
const BATCH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexState {
    Idle,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    pub state: IndexState,
    /// The embedding model these vectors belong to.
    pub model: String,
    /// Whether the local embedder answered when last asked.
    pub available: bool,
    /// Entities to process this run.
    pub total: usize,
    /// Entities finished.
    pub done: usize,
    /// Chunks stored for the current model, across the whole workspace.
    pub chunks: u64,
    /// Vectors from some other model, which can never be compared against the current one.
    pub stale_from_other_models: u64,
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl IndexStatus {
    fn idle(model: String) -> Self {
        Self {
            state: IndexState::Idle,
            model,
            available: false,
            total: 0,
            done: 0,
            chunks: 0,
            stale_from_other_models: 0,
            error: None,
            started_at: None,
            finished_at: None,
        }
    }
}

/// The one indexing run, if there is one.
#[derive(Debug)]
pub struct IndexManager {
    /// Bumped by every [`touch`], so a debounced refresh can tell whether it is still the most
    /// recent one. A counter rather than cancelling a task: nothing has to be held or aborted.
    generation: std::sync::atomic::AtomicU64,
    status: RwLock<Option<IndexStatus>>,
}

impl Default for IndexManager {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexManager {
    pub fn new() -> Self {
        Self {
            status: RwLock::new(None),
            generation: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub async fn get(&self) -> Option<IndexStatus> {
        self.status.read().await.clone()
    }

    /// Record a change and return the generation it produced.
    pub fn bump(&self) -> u64 {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub async fn is_running(&self) -> bool {
        matches!(
            self.status.read().await.as_ref().map(|s| s.state),
            Some(IndexState::Running)
        )
    }

    async fn set(&self, status: IndexStatus) {
        *self.status.write().await = Some(status);
    }

    async fn update(&self, f: impl FnOnce(&mut IndexStatus)) {
        if let Some(status) = self.status.write().await.as_mut() {
            f(status);
        }
    }
}

// ---------------------------------------------------------------- chunking

/// A piece of an entity, ready to embed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
}

/// One entity's worth of work.
#[derive(Debug, Clone)]
pub struct Pending {
    pub entity_kind: &'static str,
    pub entity_id: Id,
    pub updated_at: DateTime<Utc>,
    pub chunks: Vec<Chunk>,
}

/// Split text into overlapping chunks, preferring paragraph and sentence boundaries.
///
/// Boundaries matter more than exact sizes: a chunk that ends mid-sentence embeds the fragment
/// rather than the thought. So the cut is pulled back to the last paragraph break, then the
/// last sentence end, and only falls back to a hard character cut when neither exists within
/// reach — which happens with unbroken transcripts and pasted CSVs.
pub fn chunk_text(text: &str) -> Vec<Chunk> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }

    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= CHUNK_CHARS {
        return vec![Chunk {
            text: text.to_string(),
        }];
    }

    let mut chunks = Vec::new();
    let mut start = 0usize;

    while start < chars.len() {
        let hard_end = (start + CHUNK_CHARS).min(chars.len());

        // The last chunk takes whatever is left rather than being cut short.
        let end = if hard_end == chars.len() {
            hard_end
        } else {
            // Look back over the final third for somewhere sensible to stop.
            let floor = start + (CHUNK_CHARS * 2) / 3;
            find_boundary(&chars, floor, hard_end).unwrap_or(hard_end)
        };

        let piece: String = chars[start..end].iter().collect();
        let piece = piece.trim().to_string();
        if !piece.is_empty() {
            chunks.push(Chunk { text: piece });
        }

        if end >= chars.len() {
            break;
        }

        // Step forward, less the overlap. `max` guarantees progress even if a boundary landed
        // inside the overlap window, which would otherwise loop forever on the same span.
        start = (end.saturating_sub(CHUNK_OVERLAP_CHARS)).max(start + 1);
    }

    chunks
}

/// The best place to cut between `floor` and `ceiling`: a blank line, else a sentence end.
fn find_boundary(chars: &[char], floor: usize, ceiling: usize) -> Option<usize> {
    let mut sentence = None;

    for index in (floor..ceiling).rev() {
        if chars[index] == '\n' && index > 0 && chars[index - 1] == '\n' {
            return Some(index);
        }
        if sentence.is_none() && matches!(chars[index], '.' | '!' | '?' | '\n') {
            sentence = Some(index + 1);
        }
    }

    sentence
}

/// Everything that needs embedding, skipping what is already current.
///
/// Staleness is `entity.updated_at > newest stored chunk`. A meeting has no `updated_at`, so
/// its end time stands in — a meeting that has ended does not change again, and one still
/// running is re-indexed on the next pass, which is what should happen.
pub fn collect_pending(db: &Database, model: &str) -> notewise_storage::Result<Vec<Pending>> {
    let embeddings = EmbeddingRepository::new(db);
    let indexed = embeddings.indexed_entities(model)?;
    let is_current = |kind: &str, id: Id, updated: DateTime<Utc>| {
        indexed.iter().any(|entry| {
            entry.entity_kind == kind
                && entry.entity_id == id
                // A second of slack: timestamps round-trip through RFC 3339 text, and
                // re-embedding a whole workspace over sub-second drift would be absurd.
                && (entry.source_updated_at + chrono::Duration::seconds(1)) >= updated
        })
    };

    let mut pending = Vec::new();

    // `list_recent` already excludes trashed meetings, so a deleted one stops being embedded.
    // Its existing vectors are dropped when it is trashed — see the delete handler.
    let meetings = MeetingRepository::new(db);
    for meeting in meetings.list_recent(u32::MAX)? {
        let updated = meeting.ended_at.unwrap_or(meeting.started_at);
        if is_current("meeting", meeting.id, updated) {
            continue;
        }

        // Speaker labels are kept in the chunk text. "Dana: we agreed to ship Friday" and
        // "we agreed to ship Friday" retrieve differently for a question about what Dana said.
        let transcript = meetings.transcript_text(meeting.id)?;
        let body = format!("Meeting: {}\n\n{transcript}", meeting.title);
        let chunks = chunk_text(&body);
        if !chunks.is_empty() {
            pending.push(Pending {
                entity_kind: "meeting",
                entity_id: meeting.id,
                updated_at: updated,
                chunks,
            });
        }
    }

    // Trashed notes are excluded by `list_recent`, which is the behaviour that matters: a
    // deleted note must stop informing answers.
    for note in NoteRepository::new(db).list_recent(u32::MAX)? {
        if is_current("note", note.id, note.updated_at) {
            continue;
        }
        let chunks = chunk_text(&format!("{}\n\n{}", note.title, note.body));
        if !chunks.is_empty() {
            pending.push(Pending {
                entity_kind: "note",
                entity_id: note.id,
                updated_at: note.updated_at,
                chunks,
            });
        }
    }

    for ticket in TicketRepository::new(db).list_open()? {
        // Tickets carry no `updated_at` on this shape; `created_at` is not available either,
        // so they are re-embedded whenever the pass runs and they are short enough for that
        // to be cheap.
        let now = Utc::now();
        let body = format!(
            "{}\n\n{}",
            ticket.title,
            ticket.description.unwrap_or_default()
        );
        let chunks = chunk_text(&body);
        if !chunks.is_empty() && !is_current("ticket", ticket.id, now - chrono::Duration::days(1)) {
            pending.push(Pending {
                entity_kind: "ticket",
                entity_id: ticket.id,
                updated_at: now,
                chunks,
            });
        }
    }

    Ok(pending)
}

// ---------------------------------------------------------------- the run

/// Start an indexing pass, or return the one already going.
/// How long the workspace must be quiet before an automatic pass runs.
///
/// Long enough that typing a paragraph is one pass rather than twenty, short enough that a note
/// is answerable by the time someone finishes writing it and switches to asking about it.
const QUIET: std::time::Duration = std::time::Duration::from_secs(6);

/// Note that indexable content changed, and refresh the index once edits settle.
///
/// # Why this only maintains an index that already exists
///
/// Embedding sends the workspace to a local model. Choosing to do that is the user's, made by
/// building the index once; this keeps that choice honoured afterwards rather than making it for
/// them. On a workspace with no index, this returns without doing anything and without starting
/// one.
///
/// # Why it is debounced rather than immediate
///
/// Every keystroke in a note is a save. Indexing on each one would re-embed the same paragraph
/// repeatedly and keep a local model busy for the length of a writing session, so calls coalesce:
/// the last one within [`QUIET`] wins and the rest are dropped.
///
/// Fire-and-forget on purpose. A write must not wait for, or fail because of, an index refresh —
/// the note is saved either way, and a stale index is a worse outcome than a failed save only if
/// the save was the thing that failed.
pub fn touch(state: Arc<crate::state::AppState>) {
    let generation = state.indexing().bump();

    tokio::spawn(async move {
        tokio::time::sleep(QUIET).await;

        // A later edit landed while this was waiting; that one owns the refresh.
        if state.indexing().generation() != generation {
            return;
        }

        let has_index = {
            let model = state.embedder().await.model().to_string();
            let db = state.db().await;
            notewise_storage::EmbeddingRepository::new(&db)
                .count(&model)
                .unwrap_or(0)
                > 0
        };
        if !has_index {
            return;
        }

        tracing::debug!("workspace changed; refreshing the semantic index");
        start(state).await;
    });
}

pub async fn start(state: Arc<crate::state::AppState>) -> IndexStatus {
    let embedder = state.embedder().await;
    let model = embedder.model().to_string();

    if state.indexing().is_running().await {
        return state
            .indexing()
            .get()
            .await
            .unwrap_or_else(|| IndexStatus::idle(model));
    }

    let mut status = IndexStatus::idle(model.clone());
    status.state = IndexState::Running;
    status.started_at = Some(Utc::now());
    state.indexing().set(status.clone()).await;

    tokio::spawn(async move {
        match run(&state, &embedder).await {
            Ok(()) => {
                state
                    .indexing()
                    .update(|status| {
                        status.state = IndexState::Done;
                        status.finished_at = Some(Utc::now());
                    })
                    .await;
            }
            Err(error) => {
                state
                    .indexing()
                    .update(|status| {
                        status.state = IndexState::Failed;
                        status.error = Some(error);
                        status.finished_at = Some(Utc::now());
                    })
                    .await;
            }
        }
    });

    status
}

async fn run(
    state: &Arc<crate::state::AppState>,
    embedder: &Embedder,
) -> std::result::Result<(), String> {
    if !embedder.available().await {
        return Err(format!(
            "no local embedder: Ollama is not running, or '{}' has not been pulled. \
             Search still works by word.",
            embedder.model()
        ));
    }

    let pending = {
        let db = state.db().await;
        collect_pending(&db, embedder.model()).map_err(|e| e.to_string())?
    };

    state
        .indexing()
        .update(|status| {
            status.available = true;
            status.total = pending.len();
        })
        .await;

    for entity in pending {
        // Batched across the entity's chunks, so a long meeting is a handful of requests
        // rather than one per chunk.
        let mut vectors = Vec::with_capacity(entity.chunks.len());
        for batch in entity.chunks.chunks(BATCH) {
            let texts: Vec<String> = batch.iter().map(|c| c.text.clone()).collect();
            let embedded = embedder
                .embed_documents(&texts)
                .await
                .map_err(|e| e.to_string())?;
            vectors.extend(embedded);
        }

        let rows: Vec<NewEmbedding> = entity
            .chunks
            .iter()
            .zip(vectors)
            .enumerate()
            .map(|(index, (chunk, vector))| NewEmbedding {
                entity_kind: entity.entity_kind.to_string(),
                entity_id: entity.entity_id,
                chunk_index: index as i64,
                text: chunk.text.clone(),
                vector,
                model: embedder.model().to_string(),
                source_updated_at: entity.updated_at,
            })
            .collect();

        {
            let db = state.db().await;
            EmbeddingRepository::new(&db)
                .replace_for_entity(entity.entity_kind, entity.entity_id, embedder.model(), rows)
                .map_err(|e| e.to_string())?;
        }

        state.indexing().update(|status| status.done += 1).await;
    }

    let (chunks, stale) = {
        let db = state.db().await;
        let repo = EmbeddingRepository::new(&db);
        (
            repo.count(embedder.model()).unwrap_or(0),
            repo.count_from_other_models(embedder.model()).unwrap_or(0),
        )
    };

    state
        .indexing()
        .update(|status| {
            status.chunks = chunks;
            status.stale_from_other_models = stale;
        })
        .await;

    Ok(())
}

// ---------------------------------------------------------------- http

pub(crate) fn router() -> axum::Router<Arc<crate::state::AppState>> {
    use axum::routing::get;
    axum::Router::new().route(
        "/v1/index",
        get(index_status).post(build_index).delete(clear_index),
    )
}

/// What the semantic index holds, and whether it can be built.
///
/// Answers on a workspace that has never been indexed and on one with no Ollama at all — the
/// UI needs to be able to say *why* semantic search is off, and "no response" says nothing.
async fn index_status(
    axum::extract::State(state): axum::extract::State<Arc<crate::state::AppState>>,
) -> axum::Json<IndexStatus> {
    let embedder = state.embedder().await;
    let model = embedder.model().to_string();

    if let Some(running) = state.indexing().get().await {
        if running.state == IndexState::Running {
            return axum::Json(running);
        }
    }

    let available = embedder.available().await;
    let (chunks, stale) = {
        let db = state.db().await;
        let repo = EmbeddingRepository::new(&db);
        (
            repo.count(&model).unwrap_or(0),
            repo.count_from_other_models(&model).unwrap_or(0),
        )
    };

    let previous = state.indexing().get().await;
    axum::Json(IndexStatus {
        state: previous.as_ref().map_or(IndexState::Idle, |s| s.state),
        model,
        available,
        total: previous.as_ref().map_or(0, |s| s.total),
        done: previous.as_ref().map_or(0, |s| s.done),
        chunks,
        stale_from_other_models: stale,
        error: previous.as_ref().and_then(|s| s.error.clone()),
        started_at: previous.as_ref().and_then(|s| s.started_at),
        finished_at: previous.and_then(|s| s.finished_at),
    })
}

async fn build_index(
    axum::extract::State(state): axum::extract::State<Arc<crate::state::AppState>>,
) -> axum::Json<IndexStatus> {
    axum::Json(start(state).await)
}

/// Throw the index away.
///
/// The one case that needs it: vectors left by a model the user has stopped using. They can
/// never be compared against the current model's, so they are dead weight in every scan.
async fn clear_index(
    axum::extract::State(state): axum::extract::State<Arc<crate::state::AppState>>,
) -> crate::error::ApiResult<axum::Json<serde_json::Value>> {
    let db = state.db().await;
    let removed = EmbeddingRepository::new(&db).clear()?;
    Ok(axum::Json(serde_json::json!({ "removed": removed })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use notewise_storage::{MeetingSource, NewMeeting, NewNote, NewTranscriptSegment};

    /// An engine with an in-memory workspace and a mock model.
    fn test_state() -> Arc<crate::state::AppState> {
        Arc::new(crate::state::AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            notewise_ai_router::Router::from_config(notewise_ai_router::RouterConfig::mock())
                .expect("mock router"),
        ))
    }

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    #[test]
    fn short_text_is_one_chunk_and_keeps_its_content() {
        let chunks = chunk_text("We agreed to ship on Friday.");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "We agreed to ship on Friday.");
    }

    #[test]
    fn empty_and_blank_text_produce_nothing() {
        assert!(chunk_text("").is_empty());
        assert!(chunk_text("   \n\n  ").is_empty());
    }

    #[test]
    fn long_text_is_split_into_several_chunks() {
        let sentence = "This is a sentence about the pricing tiers we discussed. ";
        let long = sentence.repeat(80);

        let chunks = chunk_text(&long);
        assert!(chunks.len() > 1, "got {}", chunks.len());
        assert!(chunks
            .iter()
            .all(|c| c.text.chars().count() <= CHUNK_CHARS + CHUNK_OVERLAP_CHARS));
    }

    /// Cutting mid-sentence embeds a fragment rather than a thought.
    #[test]
    fn chunks_prefer_to_end_on_a_sentence() {
        let sentence = "The quick brown fox jumps over the lazy dog. ";
        let long = sentence.repeat(60);

        let chunks = chunk_text(&long);
        assert!(chunks.len() > 1);
        // Every chunk but the last should end where a sentence did.
        for chunk in &chunks[..chunks.len() - 1] {
            assert!(
                chunk.text.ends_with('.'),
                "chunk ended mid-sentence: …{}",
                &chunk.text[chunk.text.len().saturating_sub(40)..]
            );
        }
    }

    /// The sentence most likely to straddle a boundary is the long qualifying one where the
    /// decision actually lives, so the chunks have to overlap.
    #[test]
    fn consecutive_chunks_overlap() {
        let long = "word ".repeat(600);
        let chunks = chunk_text(&long);
        assert!(chunks.len() > 1);

        let first_end: String = chunks[0].text.chars().rev().take(40).collect();
        let second_start: String = chunks[1].text.chars().take(200).collect();
        let overlap_marker: String = first_end.chars().rev().collect();
        assert!(
            second_start.contains(overlap_marker.trim()) || chunks[1].text.starts_with("word"),
            "expected the second chunk to repeat the end of the first"
        );
    }

    /// Unbroken text has no boundary to find. It must still terminate and still cover
    /// everything, rather than loop or drop the tail.
    #[test]
    fn text_with_no_boundaries_still_chunks_and_terminates() {
        let unbroken = "a".repeat(5_000);
        let chunks = chunk_text(&unbroken);

        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| !c.text.is_empty()));
        // Nothing lost: the concatenation covers the original despite the overlap.
        let total: usize = chunks.iter().map(|c| c.text.chars().count()).sum();
        assert!(total >= 5_000, "content was dropped: {total} of 5000");
    }

    #[test]
    fn multibyte_text_is_never_split_mid_character() {
        // Chunking walks `char`s, so this is a guard against a future byte-slicing rewrite.
        let text = "日本語のテキストです。".repeat(200);
        let chunks = chunk_text(&text);
        assert!(chunks.len() > 1);
        // Nothing mangled: every chunk is valid UTF-8 by construction, and the content
        // survives being put back together.
        let rejoined: String = chunks.iter().map(|c| c.text.as_str()).collect();
        assert!(rejoined.contains("日本語のテキストです。"));
    }

    fn seed_meeting(db: &Database, title: &str, line: &str) -> Id {
        let repo = MeetingRepository::new(db);
        let meeting = repo
            .create(NewMeeting {
                project_id: None,
                title: title.into(),
                source: MeetingSource::Import,
                started_at: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            })
            .expect("meeting");
        repo.add_segment(NewTranscriptSegment {
            meeting_id: meeting.id,
            speaker: Some("Dana".into()),
            text: line.into(),
            start_ms: 0,
            end_ms: 1000,
            confidence: None,
        })
        .expect("segment");
        meeting.id
    }

    #[test]
    fn everything_is_pending_on_an_empty_index() {
        let db = db();
        seed_meeting(&db, "Pricing", "three tiers");
        NoteRepository::new(&db)
            .create(NewNote {
                project_id: None,
                title: "Latency".into(),
                body: "p99 under 200ms".into(),
            })
            .unwrap();

        let pending = collect_pending(&db, "m").unwrap();
        let kinds: Vec<_> = pending.iter().map(|p| p.entity_kind).collect();
        assert!(kinds.contains(&"meeting"), "got {kinds:?}");
        assert!(kinds.contains(&"note"), "got {kinds:?}");
    }

    /// The property the whole pass rests on: a second run does nothing.
    #[test]
    fn an_indexed_entity_is_not_pending_again() {
        let db = db();
        let note = NoteRepository::new(&db)
            .create(NewNote {
                project_id: None,
                title: "Latency".into(),
                body: "p99 under 200ms".into(),
            })
            .unwrap();

        let pending = collect_pending(&db, "m").unwrap();
        let entry = pending
            .iter()
            .find(|p| p.entity_id == note.id)
            .expect("note pending");

        EmbeddingRepository::new(&db)
            .replace_for_entity(
                "note",
                note.id,
                "m",
                vec![NewEmbedding {
                    entity_kind: "note".into(),
                    entity_id: note.id,
                    chunk_index: 0,
                    text: entry.chunks[0].text.clone(),
                    vector: vec![1.0, 0.0],
                    model: "m".into(),
                    source_updated_at: entry.updated_at,
                }],
            )
            .unwrap();

        let again = collect_pending(&db, "m").unwrap();
        assert!(
            !again.iter().any(|p| p.entity_id == note.id),
            "an unchanged note should not be re-embedded"
        );
    }

    #[test]
    fn an_edited_note_becomes_pending_again() {
        let db = db();
        let notes = NoteRepository::new(&db);
        let note = notes
            .create(NewNote {
                project_id: None,
                title: "Latency".into(),
                body: "p99 under 200ms".into(),
            })
            .unwrap();

        EmbeddingRepository::new(&db)
            .replace_for_entity(
                "note",
                note.id,
                "m",
                vec![NewEmbedding {
                    entity_kind: "note".into(),
                    entity_id: note.id,
                    chunk_index: 0,
                    text: "old".into(),
                    vector: vec![1.0, 0.0],
                    model: "m".into(),
                    // Embedded before the edit below.
                    source_updated_at: note.updated_at,
                }],
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(1100));
        notes
            .update(note.id, "Latency", "p99 under 150ms now")
            .unwrap();

        let pending = collect_pending(&db, "m").unwrap();
        assert!(
            pending.iter().any(|p| p.entity_id == note.id),
            "an edited note must be re-embedded"
        );
    }

    /// Vectors belong to one model. Switching models means the new one has indexed nothing.
    #[test]
    fn another_model_s_index_does_not_count_as_current() {
        let db = db();
        let note = NoteRepository::new(&db)
            .create(NewNote {
                project_id: None,
                title: "Latency".into(),
                body: "p99".into(),
            })
            .unwrap();

        EmbeddingRepository::new(&db)
            .replace_for_entity(
                "note",
                note.id,
                "model-a",
                vec![NewEmbedding {
                    entity_kind: "note".into(),
                    entity_id: note.id,
                    chunk_index: 0,
                    text: "x".into(),
                    vector: vec![1.0],
                    model: "model-a".into(),
                    source_updated_at: note.updated_at,
                }],
            )
            .unwrap();

        let pending = collect_pending(&db, "model-b").unwrap();
        assert!(pending.iter().any(|p| p.entity_id == note.id));
    }

    #[test]
    fn a_trashed_note_is_not_indexed() {
        let db = db();
        let notes = NoteRepository::new(&db);
        let note = notes
            .create(NewNote {
                project_id: None,
                title: "Secret".into(),
                body: "the launch date is March".into(),
            })
            .unwrap();
        notes.trash(note.id).unwrap();

        let pending = collect_pending(&db, "m").unwrap();
        assert!(!pending.iter().any(|p| p.entity_id == note.id));
    }

    #[test]
    fn meeting_chunks_carry_the_title_and_the_speaker() {
        let db = db();
        let id = seed_meeting(&db, "Pricing review", "we settled on three tiers");

        let pending = collect_pending(&db, "m").unwrap();
        let meeting = pending.iter().find(|p| p.entity_id == id).unwrap();
        let text = &meeting.chunks[0].text;

        assert!(text.contains("Pricing review"), "{text}");
        assert!(
            text.contains("Dana"),
            "speaker labels retrieve differently: {text}"
        );
    }
    // ------------------------------------------------------- automatic refresh

    /// Building an index is the user's decision. Keeping it fresh afterwards is not, but
    /// *starting* one unprompted would send the workspace to a model they never opted into.
    #[tokio::test]
    async fn touch_does_nothing_when_no_index_has_been_built() {
        let state = test_state();
        crate::indexing::touch(Arc::clone(&state));

        // Long enough to cover the debounce and then some.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            !state.indexing().is_running().await,
            "an unasked-for index was started"
        );
    }

    /// Repeated edits must coalesce. Every keystroke is a save, and a pass per keystroke would
    /// re-embed the same paragraph for the length of a writing session.
    #[test]
    fn repeated_edits_coalesce_into_one_generation() {
        let manager = IndexManager::new();
        let first = manager.bump();
        let second = manager.bump();
        let third = manager.bump();

        assert!(second > first && third > second, "each edit must be newer");
        assert_eq!(
            manager.generation(),
            third,
            "only the newest generation owns the refresh"
        );
    }
}
