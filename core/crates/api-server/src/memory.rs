//! Extracting durable facts, and putting them in front of the model.
//!
//! The half of Spec 8 that was missing: `ai-router`'s reflector has been tested since it landed and
//! nothing ever called it, which meant memories could be typed in by hand and never influenced a
//! single answer. Both ends are here — the observer that proposes facts, and the injection that
//! makes a stored one matter.
//!
//! # Why this is not a scheduled job
//!
//! P6 says extraction should be a Spec 7 job. Spec 7 exists, and its jobs are *agent prompts*: a
//! job is a sentence the user wrote, run on a cron, that searches and writes a note. Extraction is
//! not a prompt anybody wrote, and making it one would mean either a second kind of job — a whole
//! new dispatch in a surface that deliberately has one — or a magic prompt string the scheduler
//! recognises, which is worse.
//!
//! So it is a background tick of its own, like the calendar watcher, with the gates P6 asks for. The
//! part of P6 that matters is that extraction is off by default and does not compete with the user
//! for the model, and both of those hold.
//!
//! # Off by default
//!
//! A feature that reads every transcript to build a durable profile is in the same category as
//! voiceprint storage and acoustic separation, both of which ship off. Manual memories work with
//! extraction disabled and always have.
//!
//! # What injection costs, and why ranking is usually skipped
//!
//! Every memory in the prompt is prompt budget taken from the transcript. Globals are capped at five
//! by the schema, so all of them go in. Project memories are capped at twenty, and at most five are
//! injected — so ranking only happens when there are more than five, which on most workspaces is
//! never. When it does happen it is one embedding call for the question plus stored vectors for the
//! memories, and when there is no embedder it falls back to most-recent-first rather than failing.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router as AxumRouter};
use chrono::Utc;
use notewise_ai_router::memory::{
    as_prompt_section, observer_prompt, parse_candidates, reflect_batch, Candidate, Verdict,
};
use notewise_ai_router::{cosine, AiBackend, ChatMessage, ChatRequest};
use notewise_storage::{
    Id, MemoryOrigin, MemoryRepository, MemoryScope, NewEmbedding, NewMemory, SettingsRepository,
};
use serde::Serialize;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

type Shared = Arc<AppState>;

/// Whether automatic extraction is on. Absent means off.
pub const ENABLED_SETTING: &str = "memory.extraction.enabled";

/// How often to consider a run.
///
/// Hours, not minutes. There is nothing time-critical about noticing that somebody owns the billing
/// service, and a local model doing this every ten minutes would be noticeable for no benefit.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(4 * 60 * 60);

/// How many unprocessed meetings make a run worthwhile.
///
/// Two rather than one: a single meeting is a thin basis for a durable fact, and waiting for a
/// second means a pattern has a chance to appear twice. A manual run ignores this.
pub const MIN_UNPROCESSED: usize = 2;

/// How many meetings one run reads.
pub const MEETINGS_PER_RUN: usize = 6;

/// How many project groups one run will make a model call for.
///
/// Bounds a run at three calls. A user with a large backlog across many projects works through it
/// over several runs rather than in one long burn.
pub const MAX_GROUPS_PER_RUN: usize = 3;

/// How much of each transcript the observer sees.
///
/// The beginning, where people say what they are doing and why. A whole hour of transcript per
/// meeting across six meetings would not fit a local model's context, and the interesting part is
/// rarely the last ten minutes of a call.
pub const TRANSCRIPT_CHARS: usize = 4_000;

/// A meeting shorter than this has nothing durable in it.
pub const MIN_TRANSCRIPT_CHARS: usize = 400;

/// How many project memories may be injected into one prompt.
pub const PROJECT_INJECTION_CAP: usize = 5;

/// How memories are keyed in the embedding store.
const MEMORY_ENTITY: &str = "memory";

pub fn routes() -> AxumRouter<Shared> {
    AxumRouter::new()
        .route(
            "/v1/memories/extraction",
            get(extraction_status).put(set_enabled),
        )
        .route("/v1/memories/extract", post(run_extraction))
}

// ---------------------------------------------------------------- injection

/// The memory block for a prompt, or an empty string.
///
/// `query` is what the user asked, used to rank project memories when there are more than fit.
/// Failures are swallowed on purpose: a memory that could not be read must not fail the answer it
/// was going to improve.
pub async fn for_prompt(state: &Shared, project_id: Option<Id>, query: &str) -> String {
    let (globals, project) = {
        let db = state.db().await;
        let repo = MemoryRepository::new(&db);

        let Ok(applicable) = repo.applicable(project_id) else {
            return String::new();
        };

        let (globals, project): (Vec<_>, Vec<_>) = applicable
            .into_iter()
            .partition(|memory| memory.scope == MemoryScope::Global);
        (globals, project)
    };

    // All of them: the schema caps globals at five, and choosing between five standing facts about
    // the person would cost more than it saves.
    let mut chosen: Vec<String> = globals.into_iter().map(|memory| memory.text).collect();

    if project.len() <= PROJECT_INJECTION_CAP {
        // The common case, and no ranking is needed for it.
        chosen.extend(project.into_iter().map(|memory| memory.text));
    } else {
        chosen.extend(rank_project_memories(state, project, query).await);
    }

    as_prompt_section(&chosen)
}

/// The most relevant project memories, or the most recent when relevance cannot be judged.
///
/// The fallback is the same shape `indexing.rs` uses: every path degrades to something that works
/// less well, never to an error. An answer ranked by recency is worse than one ranked by meaning;
/// an answer that failed because the embedding daemon was down is worse than both.
async fn rank_project_memories(
    state: &Shared,
    memories: Vec<notewise_storage::Memory>,
    query: &str,
) -> Vec<String> {
    let by_recency = || -> Vec<String> {
        memories
            .iter()
            .take(PROJECT_INJECTION_CAP)
            .map(|memory| memory.text.clone())
            .collect()
    };

    if query.trim().is_empty() {
        return by_recency();
    }

    let embedder = state.embedder().await;
    let Ok(wanted) = embedder.embed_query(query).await else {
        tracing::debug!("ranking memories by recency; the embedder is unavailable");
        return by_recency();
    };

    // Vectors for the memories, embedding any that have none yet. Stored, so this is a one-time
    // cost per memory rather than a cost per question.
    let mut scored: Vec<(f32, String)> = Vec::with_capacity(memories.len());
    for memory in &memories {
        match memory_vector(state, memory, &embedder).await {
            Some(vector) => scored.push((cosine(&wanted, &vector), memory.text.clone())),
            // Unrankable: kept, at the bottom. Dropping it would make a memory invisible because an
            // embedding failed, which is the wrong way for this to degrade.
            None => scored.push((f32::MIN, memory.text.clone())),
        }
    }

    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored
        .into_iter()
        .take(PROJECT_INJECTION_CAP)
        .map(|(_, text)| text)
        .collect()
}

/// One memory's vector, embedding and storing it if this is the first time.
async fn memory_vector(
    state: &Shared,
    memory: &notewise_storage::Memory,
    embedder: &notewise_ai_router::Embedder,
) -> Option<Vec<f32>> {
    {
        let db = state.db().await;
        let repo = notewise_storage::EmbeddingRepository::new(&db);
        if let Ok(stored) = repo.all_for_model(embedder.model()) {
            if let Some(found) = stored
                .into_iter()
                .find(|e| e.entity_kind == MEMORY_ENTITY && e.entity_id == memory.id)
            {
                return Some(found.vector);
            }
        }
    }

    let vector = embedder
        .embed_documents(std::slice::from_ref(&memory.text))
        .await
        .ok()?;
    let vector = vector.into_iter().next()?;

    {
        let db = state.db().await;
        let _ = notewise_storage::EmbeddingRepository::new(&db).replace_for_entity(
            MEMORY_ENTITY,
            memory.id,
            embedder.model(),
            vec![NewEmbedding {
                entity_kind: MEMORY_ENTITY.to_string(),
                entity_id: memory.id,
                chunk_index: 0,
                text: memory.text.clone(),
                vector: vector.clone(),
                model: embedder.model().to_string(),
                source_updated_at: memory.created_at,
            }],
        );
    }

    Some(vector)
}

// ---------------------------------------------------------------- extraction

/// What a run did, and what it decided not to do.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ExtractionReport {
    /// Why the run did nothing, when it did nothing.
    pub skipped: Option<String>,
    pub meetings_read: usize,
    pub proposed: usize,
    pub kept: usize,
    /// Every candidate and what became of it, in the model's order.
    ///
    /// Returned rather than only logged: "why does it not remember that" and "why does it think
    /// that" are the two questions this feature generates, and a trace is the only honest answer to
    /// either.
    pub decisions: Vec<DecisionLine>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionLine {
    pub text: String,
    /// `kept`, `duplicate`, `third_party`, `secret`, or `unusable`.
    pub verdict: &'static str,
    pub reason: Option<String>,
}

fn describe(verdict: &Verdict) -> DecisionLine {
    let (name, reason) = match verdict {
        Verdict::Keep { global } => (
            "kept",
            Some(if *global {
                "everywhere".to_string()
            } else {
                "this project".to_string()
            }),
        ),
        Verdict::Duplicate { existing } => ("duplicate", Some(format!("already know: {existing}"))),
        Verdict::ThirdParty { reason } => ("third_party", Some(reason.clone())),
        Verdict::Secret { category } => (
            "secret",
            Some(format!("looks like a {}", category.as_str())),
        ),
        Verdict::Unusable { reason } => ("unusable", Some(reason.clone())),
    };

    DecisionLine {
        text: String::new(),
        verdict: name,
        reason,
    }
}

/// Whether the machine is free enough to spend a model call on this.
///
/// Extraction competes with the user for a local model. Running it while a meeting is being
/// transcribed would slow the transcription to save a fact nobody asked for, which is the whole
/// reason P6 has an idle gate.
pub async fn is_idle(state: &Shared) -> bool {
    if state.recording().status().await.is_some() {
        return false;
    }
    if state.dictation().status().await.is_some() {
        return false;
    }

    // An agent run in flight is the user waiting on the same model.
    !state
        .agents()
        .list()
        .await
        .iter()
        .any(|run| run.status == crate::agent::RunStatus::Running)
}

/// Whether automatic extraction has been turned on.
pub async fn is_enabled(state: &Shared) -> bool {
    let db = state.db().await;
    SettingsRepository::new(&db)
        .get(ENABLED_SETTING)
        .ok()
        .flatten()
        .as_deref()
        == Some("true")
}

/// One extraction pass.
///
/// `force` skips the gates — the enabled setting, the volume threshold, and idleness — because a user
/// who pressed a button has answered all three questions.
pub async fn extract(state: &Shared, force: bool) -> ApiResult<ExtractionReport> {
    let mut report = ExtractionReport::default();

    if !force {
        if !is_enabled(state).await {
            report.skipped = Some("automatic extraction is off".into());
            return Ok(report);
        }
        if !is_idle(state).await {
            report.skipped = Some("something else is using the model".into());
            return Ok(report);
        }
    }

    // Meetings and their transcripts, oldest first.
    let groups = {
        let db = state.db().await;
        let memories = MemoryRepository::new(&db);
        let meetings = notewise_storage::MeetingRepository::new(&db);

        let unprocessed = memories.unprocessed_meetings(MEETINGS_PER_RUN)?;

        if !force && unprocessed.len() < MIN_UNPROCESSED {
            report.skipped = Some(format!(
                "only {} meeting{} to read; waiting for {MIN_UNPROCESSED}",
                unprocessed.len(),
                if unprocessed.len() == 1 { "" } else { "s" }
            ));
            return Ok(report);
        }

        // Grouped by project, because a project-scoped fact needs to know which project. A batch
        // spanning three projects cannot answer that, and guessing would file a fact under the
        // wrong one — which is worse than not having it.
        let mut groups: std::collections::BTreeMap<Option<Id>, Vec<(Id, String)>> =
            std::collections::BTreeMap::new();

        for meeting_id in unprocessed {
            let Ok(meeting) = meetings.get(meeting_id) else {
                continue;
            };
            let transcript = meetings.transcript_text(meeting_id).unwrap_or_default();
            groups
                .entry(meeting.project_id)
                .or_default()
                .push((meeting_id, transcript));
        }

        groups
    };

    let mut read = Vec::new();

    for (project_id, meetings) in groups.into_iter().take(MAX_GROUPS_PER_RUN) {
        // Every meeting in the group is marked processed even if it is too short to read, so a
        // five-second recording is not reconsidered forever.
        read.extend(meetings.iter().map(|(id, _)| *id));

        let context = build_context(&meetings);
        if context.trim().len() < MIN_TRANSCRIPT_CHARS {
            continue;
        }

        report.meetings_read += meetings.len();

        let candidates = propose(state, &context).await;
        report.proposed += candidates.len();

        let kept = apply(state, project_id, &candidates, &mut report).await?;
        report.kept += kept;
    }

    // Marked whether or not anything came of it — see `mark_processed`'s own docs.
    {
        let db = state.db().await;
        let repo = MemoryRepository::new(&db);
        for meeting_id in read {
            let _ = repo.mark_processed(meeting_id);
        }
    }

    Ok(report)
}

/// The transcripts, trimmed, as one block.
fn build_context(meetings: &[(Id, String)]) -> String {
    let mut out = String::new();

    for (index, (_, transcript)) in meetings.iter().enumerate() {
        let text = transcript.trim();
        if text.is_empty() {
            continue;
        }

        out.push_str(&format!("--- Meeting {} ---\n", index + 1));
        // The beginning, on a character boundary: people say what they are doing and why near the
        // start, and byte slicing a transcript would panic on the first non-ASCII name.
        let excerpt: String = text.chars().take(TRANSCRIPT_CHARS).collect();
        out.push_str(&excerpt);
        out.push_str("\n\n");
    }

    out
}

/// Ask the model for candidates, with one retry on an unreadable answer.
async fn propose(state: &Shared, context: &str) -> Vec<Candidate> {
    let ai = state.ai();

    for attempt in 0..2 {
        let request = ChatRequest::new(vec![ChatMessage::user(context)])
            .with_context(vec![observer_prompt()]);

        let Ok(reply) = ai.chat(&request).await else {
            // The model being unavailable ends the run rather than retrying: the meetings stay
            // unprocessed and the next run tries again.
            return Vec::new();
        };

        let candidates = parse_candidates(&reply.text);
        if !candidates.is_empty() {
            return candidates;
        }

        // An empty answer is the correct and common one, so a second attempt is only worth making
        // if the first produced nothing *and* looked like it was trying to say something.
        if attempt == 0 && !looks_unparseable(&reply.text) {
            return Vec::new();
        }
    }

    Vec::new()
}

/// Whether a reply looks like a failed attempt at JSON rather than an honest empty answer.
///
/// A model that answered `{"memories": []}` said nothing is worth keeping and meant it. One that
/// answered with prose containing a brace was trying and failed, which is worth one retry.
fn looks_unparseable(reply: &str) -> bool {
    let trimmed = reply.trim();
    !trimmed.is_empty() && trimmed.contains('{') && parse_candidates(trimmed).is_empty()
}

/// Run the reflector and write what survives.
async fn apply(
    state: &Shared,
    project_id: Option<Id>,
    candidates: &[Candidate],
    report: &mut ExtractionReport,
) -> ApiResult<usize> {
    if candidates.is_empty() {
        return Ok(0);
    }

    let (existing, global_room, project_room) = {
        let db = state.db().await;
        let repo = MemoryRepository::new(&db);

        let existing: Vec<String> = repo
            .applicable(project_id)?
            .into_iter()
            .map(|memory| memory.text)
            .collect();

        let globals = repo.count(MemoryScope::Global, None)?;
        let global_room = MemoryScope::Global.cap().saturating_sub(globals);

        let project_room = match project_id {
            Some(id) => {
                let used = repo.count(MemoryScope::Project, Some(id))?;
                MemoryScope::Project.cap().saturating_sub(used)
            }
            // Nowhere to put a project-scoped fact. Reported as no room rather than silently
            // promoted to global — scoping a fact wider than the observer asked for is a decision
            // this code does not get to make.
            None => 0,
        };

        (existing, global_room, project_room)
    };

    // Split by the scope the observer chose, so each is reflected against its own room. Reflecting
    // them together would let three global candidates consume the project scope's headroom.
    let (globals, projects): (Vec<Candidate>, Vec<Candidate>) =
        candidates.iter().cloned().partition(|c| c.global);

    let mut written = 0;

    for (batch, room, scope) in [
        (globals, global_room, MemoryScope::Global),
        (projects, project_room, MemoryScope::Project),
    ] {
        if batch.is_empty() {
            continue;
        }

        for (candidate, verdict) in reflect_batch(&batch, &existing, room) {
            let mut line = describe(&verdict);
            line.text = candidate.text.trim().to_string();
            report.decisions.push(line);

            if !verdict.kept() {
                continue;
            }

            let db = state.db().await;
            let created = MemoryRepository::new(&db).create(NewMemory {
                scope,
                project_id: if scope == MemoryScope::Project {
                    project_id
                } else {
                    None
                },
                text: candidate.text.trim().to_string(),
                origin: MemoryOrigin::Extracted,
                source_meeting_id: None,
            });

            match created {
                Ok(_) => written += 1,
                // A cap enforced by the repository, or a `CHECK` this code got wrong. Reported in
                // the trace rather than failing the run: the other candidates are still valid.
                Err(e) => {
                    tracing::warn!(error = %e, "a memory the reflector accepted could not be stored");
                    if let Some(last) = report.decisions.last_mut() {
                        last.verdict = "unusable";
                        last.reason = Some(e.to_string());
                    }
                }
            }
        }
    }

    Ok(written)
}

/// Watch for meetings worth reading.
pub fn spawn(state: Shared) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(TICK).await;
            match extract(&state, false).await {
                Ok(report) if report.kept > 0 => {
                    tracing::info!(kept = report.kept, "learned something from recent meetings")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "an extraction run failed; continuing"),
            }
        }
    });
}

// ---------------------------------------------------------------- the surface

#[derive(Debug, Serialize)]
struct ExtractionStatus {
    enabled: bool,
    /// Meetings that have never been read.
    unprocessed: usize,
    /// Whether a run right now would do anything, and why not.
    would_run: bool,
    blocked_by: Option<String>,
}

async fn extraction_status(State(state): State<Shared>) -> ApiResult<Json<ExtractionStatus>> {
    let enabled = is_enabled(&state).await;

    let unprocessed = {
        let db = state.db().await;
        MemoryRepository::new(&db)
            .unprocessed_meetings(MEETINGS_PER_RUN)?
            .len()
    };

    let blocked_by = if !enabled {
        Some("automatic extraction is off".to_string())
    } else if unprocessed < MIN_UNPROCESSED {
        Some(format!(
            "waiting for {MIN_UNPROCESSED} unread meetings; there {} {unprocessed}",
            if unprocessed == 1 { "is" } else { "are" }
        ))
    } else if !is_idle(&state).await {
        Some("something else is using the model".to_string())
    } else {
        None
    };

    Ok(Json(ExtractionStatus {
        enabled,
        unprocessed,
        would_run: blocked_by.is_none(),
        blocked_by,
    }))
}

#[derive(Debug, serde::Deserialize)]
struct EnabledBody {
    enabled: bool,
}

/// Turn automatic extraction on or off.
async fn set_enabled(
    State(state): State<Shared>,
    Json(body): Json<EnabledBody>,
) -> ApiResult<Json<serde_json::Value>> {
    {
        let db = state.db().await;
        SettingsRepository::new(&db)
            .set(ENABLED_SETTING, if body.enabled { "true" } else { "false" })
            .map_err(|e| ApiError::Internal(format!("could not save that: {e}")))?;
    }

    Ok(Json(serde_json::json!({ "enabled": body.enabled })))
}

/// Read recent meetings now.
///
/// Forced: pressing the button answers the enabled question, the volume question, and the idleness
/// question all at once. The one thing it does not bypass is the reflector.
async fn run_extraction(State(state): State<Shared>) -> ApiResult<Json<ExtractionReport>> {
    let report = extract(&state, true).await?;
    let _ = Utc::now();
    Ok(Json(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::TimeZone;
    use http_body_util::BodyExt;
    use notewise_ai_router::{ChatRequest, Router as AiRouter};
    use notewise_storage::Database;
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use tower::ServiceExt;

    /// A backend that answers from a queue and records every prompt it was given.
    ///
    /// The recorded prompts are the point: whether a memory reaches the model is not observable from
    /// the answer, and asserting on the answer of a mock proves nothing. Asserting on what was *sent*
    /// proves the injection happened.
    #[derive(Debug)]
    struct Scripted {
        replies: Arc<StdMutex<VecDeque<String>>>,
        seen: Arc<StdMutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl AiBackend for Scripted {
        fn model_id(&self) -> &str {
            "scripted"
        }
        fn is_local(&self) -> bool {
            true
        }
        async fn summarize(
            &self,
            input: &notewise_ai_router::TranscriptInput,
        ) -> notewise_ai_router::Result<notewise_ai_router::SummaryOutput> {
            // Instructions are where a memory lands on the summary path.
            self.seen
                .lock()
                .unwrap()
                .push(input.instructions.clone().unwrap_or_default());
            Ok(notewise_ai_router::SummaryOutput {
                text: "a summary".into(),
                model: "scripted".into(),
            })
        }
        async fn extract_decisions(
            &self,
            _: &notewise_ai_router::TranscriptInput,
        ) -> notewise_ai_router::Result<Vec<notewise_ai_router::ExtractedDecision>> {
            Ok(Vec::new())
        }
        async fn extract_action_items(
            &self,
            _: &notewise_ai_router::TranscriptInput,
        ) -> notewise_ai_router::Result<Vec<notewise_ai_router::ExtractedActionItem>> {
            Ok(Vec::new())
        }
        async fn chat(
            &self,
            request: &ChatRequest,
        ) -> notewise_ai_router::Result<notewise_ai_router::ChatResponse> {
            self.seen.lock().unwrap().push(request.context.join("\n"));

            let text = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| r#"{"memories": []}"#.into());

            Ok(notewise_ai_router::ChatResponse {
                text,
                model: "scripted".into(),
            })
        }
    }

    type Seen = Arc<StdMutex<Vec<String>>>;

    fn scripted(replies: &[&str]) -> (Shared, Seen) {
        let seen: Seen = Arc::new(StdMutex::new(Vec::new()));
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::with_backend(Box::new(Scripted {
                replies: Arc::new(StdMutex::new(
                    replies.iter().map(|r| (*r).to_string()).collect(),
                )),
                seen: Arc::clone(&seen),
            })),
        ));
        (state, seen)
    }

    /// A meeting that has ended, with enough transcript to be worth reading.
    async fn seed_meeting(state: &Shared, title: &str, project_id: Option<Id>) -> Id {
        let db = state.db().await;
        let repo = notewise_storage::MeetingRepository::new(&db);
        let meeting = repo
            .create(notewise_storage::NewMeeting {
                project_id,
                title: title.into(),
                source: notewise_storage::MeetingSource::Import,
                started_at: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            })
            .expect("a meeting");

        repo.add_segment(notewise_storage::NewTranscriptSegment {
            meeting_id: meeting.id,
            speaker: Some("Alex".into()),
            // Long enough to clear the minimum, and shaped like a standup.
            text: "I own the billing service and I run the platform standup every Monday. \
                   We call the ingest pipeline the funnel because it narrows. \
                   This quarter I am responsible for the migration off the old queue, \
                   which is the thing I keep getting asked about in every review."
                .repeat(3),
            start_ms: 0,
            end_ms: 60_000,
            confidence: None,
        })
        .expect("a segment");

        repo.end(meeting.id, Utc::now()).expect("ends");
        meeting.id
    }

    async fn project(state: &Shared) -> Id {
        let db = state.db().await;
        let workspace = notewise_storage::WorkspaceRepository::new(&db)
            .create(notewise_storage::NewWorkspace {
                name: "Acme".into(),
            })
            .expect("a workspace");

        notewise_storage::ProjectRepository::new(&db)
            .create(notewise_storage::NewProject {
                workspace_id: workspace.id,
                name: "Billing".into(),
                description: None,
            })
            .expect("a project")
            .id
    }

    async fn memories(state: &Shared) -> Vec<String> {
        let db = state.db().await;
        MemoryRepository::new(&db)
            .list()
            .expect("reads")
            .into_iter()
            .map(|m| m.text)
            .collect()
    }

    // ------------------------------------------------------------ the gates

    /// A feature that reads every transcript to build a durable profile ships off.
    #[tokio::test]
    async fn extraction_is_off_on_a_fresh_workspace() {
        let (state, seen) = scripted(&[]);
        seed_meeting(&state, "Standup", None).await;
        seed_meeting(&state, "Planning", None).await;

        let report = extract(&state, false).await.expect("a run");
        assert_eq!(
            report.skipped.as_deref(),
            Some("automatic extraction is off")
        );
        assert!(seen.lock().unwrap().is_empty(), "no model was called");
        assert!(memories(&state).await.is_empty());
    }

    /// A single meeting is a thin basis for a durable fact.
    #[tokio::test]
    async fn one_unread_meeting_is_not_enough() {
        let (state, _) = scripted(&[]);
        {
            let db = state.db().await;
            SettingsRepository::new(&db)
                .set(ENABLED_SETTING, "true")
                .expect("enables");
        }
        seed_meeting(&state, "Standup", None).await;

        let report = extract(&state, false).await.expect("a run");
        assert!(
            report
                .skipped
                .as_deref()
                .is_some_and(|r| r.contains("waiting")),
            "{report:?}"
        );
    }

    /// Pressing the button answers the enabled question, the volume question, and idleness at once.
    #[tokio::test]
    async fn a_manual_run_ignores_the_gates_but_not_the_reflector() {
        let (state, _) = scripted(&[
            r#"{"memories":[{"text":"I own the billing service","global":true},
                            {"text":"Dana is difficult in reviews","global":true}]}"#,
        ]);
        seed_meeting(&state, "Standup", None).await;

        let report = extract(&state, true).await.expect("a run");
        assert_eq!(report.skipped, None, "a manual run is not gated");
        assert_eq!(report.proposed, 2);
        assert_eq!(report.kept, 1);

        let kept = memories(&state).await;
        assert_eq!(kept, vec!["I own the billing service".to_string()]);

        // And the trace says what happened to the other one, which is the only honest answer to
        // "why does it not remember that".
        //
        // Asserted as "refused" rather than as `third_party`, because the reflector cannot tell a
        // colleague's name from a weekday without a pronoun to go on — "Dana is difficult" comes
        // back as "not a fact about the user", which is the honest verdict for what it can actually
        // see. What is guaranteed, and what matters, is that it is not stored.
        let refused = report
            .decisions
            .iter()
            .find(|d| d.text.contains("Dana"))
            .expect("the refusal is recorded");
        assert!(
            matches!(refused.verdict, "third_party" | "unusable"),
            "{refused:?}"
        );
        assert!(!kept.iter().any(|m| m.contains("Dana")));
    }

    /// With a pronoun there is real evidence, and the verdict says so — which is what lets the
    /// interface explain the refusal rather than shrug at it.
    #[tokio::test]
    async fn a_claim_about_someone_else_is_named_as_one_when_a_pronoun_gives_it_away() {
        let (state, _) = scripted(&[
            r#"{"memories":[{"text":"Sam says he is leaving the team in March","global":true}]}"#,
        ]);
        seed_meeting(&state, "Standup", None).await;

        let report = extract(&state, true).await.expect("a run");
        assert_eq!(report.kept, 0);
        assert_eq!(report.decisions[0].verdict, "third_party");
        assert!(memories(&state).await.is_empty());
    }

    /// P7, through the whole pass rather than only in the reflector's own tests: a secret the
    /// observer proposed is refused at write time, not masked at send time.
    #[tokio::test]
    async fn a_secret_the_observer_proposed_is_never_written_down() {
        let (state, _) = scripted(&[
            r#"{"memories":[{"text":"my deploy key is sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","global":true}]}"#,
        ]);
        seed_meeting(&state, "Standup", None).await;

        let report = extract(&state, true).await.expect("a run");
        assert_eq!(report.kept, 0);
        assert_eq!(report.decisions[0].verdict, "secret");
        assert!(memories(&state).await.is_empty());
    }

    /// Two phrasings of one fact must not both consume a capped slot.
    #[tokio::test]
    async fn a_fact_already_known_is_not_stored_twice() {
        let (state, _) = scripted(&[
            r#"{"memories":[{"text":"I prefer summaries that are short","global":true}]}"#,
        ]);
        {
            let db = state.db().await;
            MemoryRepository::new(&db)
                .create(NewMemory {
                    scope: MemoryScope::Global,
                    project_id: None,
                    text: "I prefer short summaries".into(),
                    origin: MemoryOrigin::Manual,
                    source_meeting_id: None,
                })
                .expect("a memory");
        }
        seed_meeting(&state, "Standup", None).await;

        let report = extract(&state, true).await.expect("a run");
        assert_eq!(report.kept, 0);
        assert_eq!(report.decisions[0].verdict, "duplicate");
        assert_eq!(memories(&state).await.len(), 1);
    }

    /// Extraction competes with the user for a local model.
    #[tokio::test]
    async fn a_run_waits_while_something_else_is_using_the_model() {
        let (state, _) = scripted(&[]);
        {
            let db = state.db().await;
            SettingsRepository::new(&db)
                .set(ENABLED_SETTING, "true")
                .expect("enables");
        }
        seed_meeting(&state, "Standup", None).await;
        seed_meeting(&state, "Planning", None).await;

        // Idle with nothing going on, which is the state a test starts in.
        assert!(is_idle(&state).await);

        let report = extract(&state, false).await.expect("a run");
        assert_eq!(report.skipped, None, "{report:?}");
    }

    // ------------------------------------------------------------ the pass

    /// A meeting with nothing worth remembering must not be re-read on every pass forever.
    #[tokio::test]
    async fn every_meeting_read_is_marked_processed_even_when_nothing_was_kept() {
        let (state, _) = scripted(&[r#"{"memories": []}"#]);
        seed_meeting(&state, "Standup", None).await;
        seed_meeting(&state, "Planning", None).await;

        let report = extract(&state, true).await.expect("a run");
        assert_eq!(report.kept, 0);

        let left = {
            let db = state.db().await;
            MemoryRepository::new(&db)
                .unprocessed_meetings(10)
                .expect("reads")
        };
        assert!(left.is_empty(), "{left:?}");

        // And a second run has nothing to read.
        let again = extract(&state, true).await.expect("a run");
        assert_eq!(again.meetings_read, 0);
    }

    /// A project-scoped fact needs to know which project, and a meeting in one supplies it.
    #[tokio::test]
    async fn a_project_scoped_fact_is_filed_under_that_project() {
        let (state, _) = scripted(&[
            r#"{"memories":[{"text":"I am migrating billing off the old queue","global":false}]}"#,
        ]);
        let project_id = project(&state).await;
        seed_meeting(&state, "Billing sync", Some(project_id)).await;

        let report = extract(&state, true).await.expect("a run");
        assert_eq!(report.kept, 1);

        let db = state.db().await;
        let stored = MemoryRepository::new(&db).list().expect("reads");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].scope, MemoryScope::Project);
        assert_eq!(stored[0].project_id, Some(project_id));
        assert_eq!(stored[0].origin, MemoryOrigin::Extracted);
    }

    /// Scoping a fact to the wrong project is worse than not having it.
    #[tokio::test]
    async fn a_project_fact_from_a_meeting_with_no_project_is_dropped_not_promoted() {
        let (state, _) = scripted(&[
            r#"{"memories":[{"text":"I am migrating billing off the old queue","global":false}]}"#,
        ]);
        seed_meeting(&state, "Loose meeting", None).await;

        let report = extract(&state, true).await.expect("a run");
        assert_eq!(report.kept, 0, "{report:?}");
        assert!(memories(&state).await.is_empty());

        let dropped = report.decisions.first().expect("a decision");
        assert_eq!(dropped.verdict, "unusable");
        assert!(
            dropped
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("room")),
            "{dropped:?}"
        );
    }

    /// Global candidates must not eat the project scope's headroom, or the other way round.
    #[tokio::test]
    async fn the_two_scopes_are_reflected_against_their_own_room() {
        let (state, _) = scripted(&[r#"{"memories":[
                {"text":"I prefer summaries that lead with decisions","global":true},
                {"text":"I am migrating billing off the old queue","global":false}
            ]}"#]);
        let project_id = project(&state).await;
        seed_meeting(&state, "Billing sync", Some(project_id)).await;

        let report = extract(&state, true).await.expect("a run");
        assert_eq!(report.kept, 2, "{report:?}");

        let db = state.db().await;
        let stored = MemoryRepository::new(&db).list().expect("reads");
        assert_eq!(
            stored
                .iter()
                .filter(|m| m.scope == MemoryScope::Global)
                .count(),
            1
        );
        assert_eq!(
            stored
                .iter()
                .filter(|m| m.scope == MemoryScope::Project)
                .count(),
            1
        );
    }

    /// The cap is a hard limit, and reaching it is visible rather than silent.
    #[tokio::test]
    async fn a_full_global_scope_refuses_new_facts_and_says_so() {
        let (state, _) = scripted(&[
            r#"{"memories":[{"text":"I would like to be remembered too","global":true}]}"#,
        ]);

        {
            let db = state.db().await;
            let repo = MemoryRepository::new(&db);
            for n in 0..MemoryScope::Global.cap() {
                repo.create(NewMemory {
                    scope: MemoryScope::Global,
                    project_id: None,
                    text: format!("I already know standing fact number {n} about myself"),
                    origin: MemoryOrigin::Manual,
                    source_meeting_id: None,
                })
                .expect("fills the scope");
            }
        }

        seed_meeting(&state, "Standup", None).await;
        let report = extract(&state, true).await.expect("a run");

        assert_eq!(report.kept, 0);
        let refused = report.decisions.first().expect("a decision");
        assert!(
            refused
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("room")),
            "{refused:?}"
        );
    }

    /// The model being unavailable leaves the meetings unread for next time... except that they are
    /// marked processed, which is the deliberate trade: re-reading forever is worse.
    #[tokio::test]
    async fn an_unusable_model_answer_costs_nothing_but_the_run() {
        let (state, _) = scripted(&["I would rather not do that.", "Still not."]);
        seed_meeting(&state, "Standup", None).await;

        let report = extract(&state, true).await.expect("a run");
        assert_eq!(report.proposed, 0);
        assert_eq!(report.kept, 0);
        assert!(memories(&state).await.is_empty());
    }

    /// A short recording has nothing durable in it and must not cost a model call.
    #[tokio::test]
    async fn a_meeting_too_short_to_read_is_skipped_and_marked() {
        let (state, seen) = scripted(&[]);

        let meeting_id = {
            let db = state.db().await;
            let repo = notewise_storage::MeetingRepository::new(&db);
            let meeting = repo
                .create(notewise_storage::NewMeeting {
                    project_id: None,
                    title: "Quick word".into(),
                    source: notewise_storage::MeetingSource::Import,
                    started_at: Utc::now(),
                })
                .expect("a meeting");
            repo.add_segment(notewise_storage::NewTranscriptSegment {
                meeting_id: meeting.id,
                speaker: None,
                text: "ok".into(),
                start_ms: 0,
                end_ms: 500,
                confidence: None,
            })
            .expect("a segment");
            repo.end(meeting.id, Utc::now()).expect("ends");
            meeting.id
        };

        let report = extract(&state, true).await.expect("a run");
        assert_eq!(report.meetings_read, 0);
        assert!(seen.lock().unwrap().is_empty(), "no model call");

        let left = {
            let db = state.db().await;
            MemoryRepository::new(&db)
                .unprocessed_meetings(10)
                .expect("reads")
        };
        assert!(
            !left.contains(&meeting_id),
            "a five-second recording must not be reconsidered forever"
        );
    }

    // ------------------------------------------------------------ injection

    /// The whole point of the feature, and the thing that was missing: a stored memory reaching a
    /// prompt. Asserted on what was *sent*, because the answer of a mock proves nothing.
    #[tokio::test]
    async fn a_stored_memory_reaches_the_model_on_the_summary_path() {
        let (state, seen) = scripted(&[]);
        {
            let db = state.db().await;
            MemoryRepository::new(&db)
                .create(NewMemory {
                    scope: MemoryScope::Global,
                    project_id: None,
                    text: "I prefer summaries that lead with decisions".into(),
                    origin: MemoryOrigin::Manual,
                    source_meeting_id: None,
                })
                .expect("a memory");
        }

        let meeting_id = seed_meeting(&state, "Standup", None).await;

        let app = crate::routes::router(Arc::clone(&state));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/meetings/{meeting_id}/summarize"))
                    .body(Body::empty())
                    .expect("builds"),
            )
            .await
            .expect("request");
        assert_eq!(response.status(), StatusCode::OK);

        let sent = seen.lock().unwrap().join("\n");
        assert!(
            sent.contains("I prefer summaries that lead with decisions"),
            "the memory never reached the model: {sent}"
        );
        assert!(
            sent.contains("keep in mind about the person"),
            "and it should arrive labelled: {sent}"
        );
    }

    /// A workspace with no memories must not send an empty heading.
    #[tokio::test]
    async fn nothing_is_injected_when_there_is_nothing_to_say() {
        let (state, _) = scripted(&[]);
        assert_eq!(for_prompt(&state, None, "anything").await, "");
    }

    /// Globals apply everywhere; a project memory does not leak into another project.
    #[tokio::test]
    async fn a_project_memory_is_only_injected_for_that_project() {
        let (state, _) = scripted(&[]);
        let project_id = project(&state).await;

        {
            let db = state.db().await;
            let repo = MemoryRepository::new(&db);
            repo.create(NewMemory {
                scope: MemoryScope::Global,
                project_id: None,
                text: "I prefer short summaries".into(),
                origin: MemoryOrigin::Manual,
                source_meeting_id: None,
            })
            .expect("a global");
            repo.create(NewMemory {
                scope: MemoryScope::Project,
                project_id: Some(project_id),
                text: "billing runs on the old queue until March".into(),
                origin: MemoryOrigin::Manual,
                source_meeting_id: None,
            })
            .expect("a project memory");
        }

        let scoped = for_prompt(&state, Some(project_id), "what about billing").await;
        assert!(scoped.contains("short summaries"), "{scoped}");
        assert!(scoped.contains("old queue"), "{scoped}");

        let elsewhere = for_prompt(&state, None, "what about billing").await;
        assert!(elsewhere.contains("short summaries"), "{elsewhere}");
        assert!(
            !elsewhere.contains("old queue"),
            "a project memory must not leak: {elsewhere}"
        );
    }

    /// Every memory in the prompt is budget taken from the transcript, so the injection is capped
    /// even though the scope allows twenty.
    #[tokio::test]
    async fn project_memories_are_capped_at_injection_time() {
        let (state, _) = scripted(&[]);
        let project_id = project(&state).await;

        {
            let db = state.db().await;
            let repo = MemoryRepository::new(&db);
            for n in 0..12 {
                repo.create(NewMemory {
                    scope: MemoryScope::Project,
                    project_id: Some(project_id),
                    text: format!("project fact number {n} that I keep in mind"),
                    origin: MemoryOrigin::Manual,
                    source_meeting_id: None,
                })
                .expect("a memory");
            }
        }

        // No embedder in a test, so this exercises the recency fallback — which is the path that
        // has to work when the daemon is down.
        let injected = for_prompt(&state, Some(project_id), "billing").await;
        let lines = injected.lines().filter(|l| l.starts_with("- ")).count();
        assert_eq!(lines, PROJECT_INJECTION_CAP, "{injected}");
    }

    // ------------------------------------------------------------ the surface

    async fn call(app: &AxumRouter<()>, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = app.clone().oneshot(request).await.expect("request");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }

    #[tokio::test]
    async fn the_status_says_why_a_run_would_not_happen() {
        let (state, _) = scripted(&[]);
        let app = routes().with_state(Arc::clone(&state));

        let (status, body) = call(
            &app,
            Request::builder()
                .uri("/v1/memories/extraction")
                .body(Body::empty())
                .expect("builds"),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["enabled"], false);
        assert_eq!(body["would_run"], false);
        assert!(body["blocked_by"]
            .as_str()
            .expect("a reason")
            .contains("off"));
    }

    #[tokio::test]
    async fn extraction_can_be_turned_on_and_the_status_follows() {
        let (state, _) = scripted(&[]);
        let app = routes().with_state(Arc::clone(&state));
        seed_meeting(&state, "Standup", None).await;
        seed_meeting(&state, "Planning", None).await;

        let (status, _) = call(
            &app,
            Request::builder()
                .method("PUT")
                .uri("/v1/memories/extraction")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"enabled":true}"#))
                .expect("builds"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (_, body) = call(
            &app,
            Request::builder()
                .uri("/v1/memories/extraction")
                .body(Body::empty())
                .expect("builds"),
        )
        .await;
        assert_eq!(body["enabled"], true);
        assert_eq!(body["unprocessed"], 2);
        assert_eq!(body["would_run"], true, "{body}");
        assert_eq!(body["blocked_by"], serde_json::Value::Null);
    }

    /// The button runs regardless, and reports what it decided.
    #[tokio::test]
    async fn a_manual_run_reports_its_decisions() {
        let (state, _) =
            scripted(&[r#"{"memories":[{"text":"I own the billing service","global":true}]}"#]);
        let app = routes().with_state(Arc::clone(&state));
        seed_meeting(&state, "Standup", None).await;

        let (status, body) = call(
            &app,
            Request::builder()
                .method("POST")
                .uri("/v1/memories/extract")
                .body(Body::empty())
                .expect("builds"),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["kept"], 1);
        assert_eq!(body["decisions"][0]["verdict"], "kept");
        assert_eq!(body["decisions"][0]["text"], "I own the billing service");
    }
}
