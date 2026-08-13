//! HTTP route table and handlers.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router as AxumRouter};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use notewise_ai_router::{
    suggest_questions, AiBackend, BackendKind, ChatMessage, ChatRequest, ClarifierConfig,
    ClarifierSession, Role, TranscriptInput, Utterance,
};
use notewise_transcription::{ModelRegistry, ModelStore};
use notewise_graph::{EdgeKind, Graph, NodeKind, NodeRef};
use notewise_storage::{
    meeting_to_markdown, ExportOptions, Id, Meeting, MeetingRepository, MeetingSource,
    NewMeeting, NewNote, NewSummary,
    NewTranscriptSegment, Note, NoteRepository, SearchRepository, SummaryRepository, Ticket,
    TicketRepository, TranscriptSegment,
};

use crate::error::{ApiError, ApiResult};
use crate::recording::{self, RecordingError, StartRequest};
use crate::state::AppState;

type Shared = Arc<AppState>;

pub(crate) fn router(state: Shared) -> AxumRouter {
    AxumRouter::new()
        .route("/health", get(health))
        .route("/v1/meetings", get(list_meetings).post(create_meeting))
        .route("/v1/meetings/:id", get(get_meeting))
        .route("/v1/meetings/:id/end", post(end_meeting))
        .route(
            "/v1/meetings/:id/transcript",
            get(get_transcript).post(append_segments),
        )
        .route("/v1/meetings/:id/summarize", post(summarize_meeting))
        .route("/v1/meetings/:id/related", get(related_to_meeting))
        .route("/v1/meetings/:id/export", get(export_meeting))
        .route("/v1/notes", get(list_notes).post(create_note))
        .route("/v1/tickets", get(list_tickets))
        .route("/v1/meetings/:id/questions", post(clarifying_questions))
        .route("/v1/meetings/:id/chat", post(chat_about_meeting))
        .route("/v1/backends", get(list_backends))
        .route("/v1/models", get(list_models))
        .route("/v1/models/:name/download", post(download_model))
        .route("/v1/search", get(search))
        .route(
            "/v1/recording",
            get(recording_status).post(start_recording).delete(stop_recording),
        )
        .with_state(state)
}

/// Parse a path id, turning a malformed one into a 400 rather than a 500.
fn parse_id(raw: &str) -> ApiResult<Id> {
    raw.parse()
        .map_err(|_| ApiError::BadRequest(format!("'{raw}' is not a valid id")))
}

// ---------------------------------------------------------------- health

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    schema_version: u32,
    /// Whether the configured AI backend keeps data on this machine. Surfaced so a client
    /// can show the user where their transcripts are going.
    ai_local: bool,
    ai_model: String,
    /// Whether this build can capture audio.
    ///
    /// Reported rather than assumed: capture is behind compile-time features, so a client has
    /// no other way to know. A UI that guessed would show a record button that did nothing.
    can_record: bool,
    /// Whether a recording is in progress, so a reloaded window recovers the live state.
    recording_meeting_id: Option<Id>,
}

async fn health(State(state): State<Shared>) -> ApiResult<Json<Health>> {
    let schema_version = state.db().await.schema_version()?;
    Ok(Json(Health {
        status: "ok",
        schema_version,
        ai_local: state.ai().is_local(),
        ai_model: state.ai().model_id().to_string(),
        // Recording also needs a file-backed database, so an `--ephemeral` engine correctly
        // reports that it cannot record even in a build that otherwise could.
        can_record: recording::SUPPORTED && state.db_path().is_some(),
        recording_meeting_id: state.recording().status().await.map(|s| s.meeting_id),
    }))
}

// ---------------------------------------------------------------- recording

#[derive(Debug, Deserialize)]
struct StartRecordingBody {
    title: Option<String>,
    /// Input device name. Omit for the system default.
    device: Option<String>,
    /// Transcription model, e.g. `base.en`. Omit for the default.
    model: Option<String>,
    /// Separate speakers when the recording stops. Defaults to on.
    diarize: Option<bool>,
}

#[derive(Debug, Serialize)]
struct RecordingStatusBody {
    recording: bool,
    meeting_id: Option<Id>,
    device: Option<String>,
    model: Option<String>,
    /// So a client can tell "not recording" from "cannot record".
    can_record: bool,
}

#[derive(Debug, Serialize)]
struct StoppedBody {
    meeting_id: Id,
    segments: usize,
    speakers: usize,
    audio_ms: i64,
}

async fn recording_status(State(state): State<Shared>) -> Json<RecordingStatusBody> {
    let status = state.recording().status().await;
    Json(RecordingStatusBody {
        recording: status.is_some(),
        meeting_id: status.as_ref().map(|s| s.meeting_id),
        device: status.as_ref().map(|s| s.device.clone()),
        model: status.as_ref().map(|s| s.model.clone()),
        can_record: recording::SUPPORTED && state.db_path().is_some(),
    })
}

/// Start recording, creating the meeting in the same call.
async fn start_recording(
    State(state): State<Shared>,
    body: Option<Json<StartRecordingBody>>,
) -> ApiResult<(axum::http::StatusCode, Json<RecordingStatusBody>)> {
    let body = body.map(|Json(b)| b);

    let status = state
        .recording()
        .start(
            state.db_path().map(|p| p.to_path_buf()),
            state.model_dir().to_path_buf(),
            StartRequest {
                title: body.as_ref().and_then(|b| b.title.clone()),
                device: body.as_ref().and_then(|b| b.device.clone()),
                model: body.as_ref().and_then(|b| b.model.clone()),
                diarize: body.as_ref().and_then(|b| b.diarize).unwrap_or(true),
            },
        )
        .await?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(RecordingStatusBody {
            recording: true,
            meeting_id: Some(status.meeting_id),
            device: Some(status.device),
            model: Some(status.model),
            can_record: true,
        }),
    ))
}

/// Stop the active recording and report what it captured.
///
/// `DELETE` rather than `POST /stop`: the recording is a resource that either exists or does
/// not, which also makes a duplicate stop a clean 409 instead of an ambiguous success.
async fn stop_recording(State(state): State<Shared>) -> ApiResult<Json<StoppedBody>> {
    let (meeting_id, outcome) = state.recording().stop().await?;
    Ok(Json(StoppedBody {
        meeting_id,
        segments: outcome.segments,
        speakers: outcome.speakers,
        audio_ms: outcome.audio_ms,
    }))
}

impl From<RecordingError> for ApiError {
    fn from(error: RecordingError) -> Self {
        match error {
            // 501: the request was valid, this build just cannot do it. A 400 would suggest
            // the caller was wrong, and a 500 would suggest a bug.
            RecordingError::Unsupported => ApiError::NotImplemented(error.to_string()),
            RecordingError::Ephemeral => ApiError::BadRequest(error.to_string()),
            RecordingError::AlreadyRecording(_) | RecordingError::NotRecording => {
                ApiError::Conflict(error.to_string())
            }
            RecordingError::Failed(message) => ApiError::Internal(message),
        }
    }
}

// ---------------------------------------------------------------- meetings

#[derive(Debug, Deserialize)]
struct ListQuery {
    limit: Option<u32>,
}

impl ListQuery {
    /// Clamped so a client cannot ask for the entire history in one response.
    fn limit(&self) -> u32 {
        self.limit.unwrap_or(50).clamp(1, 500)
    }
}

async fn list_meetings(
    State(state): State<Shared>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<Meeting>>> {
    let db = state.db().await;
    Ok(Json(
        MeetingRepository::new(&db).list_recent(query.limit())?,
    ))
}

#[derive(Debug, Deserialize)]
struct CreateMeeting {
    title: String,
    #[serde(default)]
    project_id: Option<Id>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    started_at: Option<DateTime<Utc>>,
}

async fn create_meeting(
    State(state): State<Shared>,
    Json(body): Json<CreateMeeting>,
) -> ApiResult<Json<Meeting>> {
    let source = match body.source.as_deref() {
        None => MeetingSource::Combined,
        Some(raw) => MeetingSource::parse(raw)
            .ok_or_else(|| ApiError::BadRequest(format!("unknown meeting source '{raw}'")))?,
    };

    let db = state.db().await;
    let meeting = MeetingRepository::new(&db).create(NewMeeting {
        project_id: body.project_id,
        title: body.title,
        source,
        started_at: body.started_at.unwrap_or_else(Utc::now),
    })?;

    // A meeting inside a project is contained by it — record that as an edge so the
    // project's rollup is a traversal rather than a bespoke query.
    if let Some(project_id) = meeting.project_id {
        Graph::new(&db).connect(
            NodeRef::new(NodeKind::Project, project_id),
            EdgeKind::Contains,
            NodeRef::new(NodeKind::Meeting, meeting.id),
        )?;
    }

    Ok(Json(meeting))
}

async fn get_meeting(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Meeting>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    Ok(Json(MeetingRepository::new(&db).get(id)?))
}

async fn end_meeting(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Meeting>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    Ok(Json(MeetingRepository::new(&db).end(id, Utc::now())?))
}

// ---------------------------------------------------------------- transcript

async fn get_transcript(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<TranscriptSegment>>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    let repo = MeetingRepository::new(&db);
    // Confirm the meeting exists so an unknown id is a 404 rather than an empty list.
    repo.get(id)?;
    Ok(Json(repo.segments(id)?))
}

#[derive(Debug, Deserialize)]
struct NewSegment {
    text: String,
    start_ms: i64,
    end_ms: i64,
    #[serde(default)]
    speaker: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
}

#[derive(Debug, Serialize)]
struct AppendedSegments {
    appended: usize,
    ids: Vec<Id>,
}

/// Append transcript segments.
///
/// Batched because transcription emits segments in bursts; one request per segment would
/// dominate the recording path.
async fn append_segments(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<Vec<NewSegment>>,
) -> ApiResult<Json<AppendedSegments>> {
    let meeting_id = parse_id(&id)?;

    if body.is_empty() {
        return Err(ApiError::BadRequest("no segments supplied".into()));
    }
    if let Some(bad) = body.iter().find(|s| s.end_ms < s.start_ms) {
        return Err(ApiError::BadRequest(format!(
            "segment ends before it starts ({} < {})",
            bad.end_ms, bad.start_ms
        )));
    }

    let db = state.db().await;
    let repo = MeetingRepository::new(&db);
    repo.get(meeting_id)?;

    let ids = repo.add_segments(
        body.into_iter()
            .map(|s| NewTranscriptSegment {
                meeting_id,
                speaker: s.speaker,
                text: s.text,
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                confidence: s.confidence,
            })
            .collect(),
    )?;

    Ok(Json(AppendedSegments {
        appended: ids.len(),
        ids,
    }))
}

// ---------------------------------------------------------------- summarize

#[derive(Debug, Serialize)]
struct SummarizeResponse {
    summary_id: Id,
    text: String,
    model: String,
    decisions: usize,
    action_items: usize,
}

/// Summarize a meeting, persist the results, and wire them into the graph.
///
/// The database lock is deliberately released before the model call — summarization can take
/// tens of seconds, and holding the lock across it would stall every other request.
async fn summarize_meeting(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<SummarizeResponse>> {
    let meeting_id = parse_id(&id)?;

    let (title, transcript) = {
        let db = state.db().await;
        let repo = MeetingRepository::new(&db);
        let meeting = repo.get(meeting_id)?;
        (meeting.title, repo.transcript_text(meeting_id)?)
    }; // lock released here, before any model call

    if transcript.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "meeting has no transcript to summarize".into(),
        ));
    }

    let input = TranscriptInput::new(title, transcript);
    let summary = state.ai().summarize(&input).await?;
    let decisions = state.ai().extract_decisions(&input).await?;
    let action_items = state.ai().extract_action_items(&input).await?;

    let db = state.db().await;
    let repo = SummaryRepository::new(&db);
    let stored = repo.create(NewSummary {
        meeting_id,
        text: summary.text.clone(),
        model: summary.model.clone(),
    })?;

    for decision in &decisions {
        repo.add_decision(notewise_storage::NewDecision {
            summary_id: stored.id,
            text: decision.text.clone(),
            reasoning: decision.reasoning.clone(),
            decided_at: None,
        })?;
    }
    for item in &action_items {
        repo.add_action_item(notewise_storage::NewActionItem {
            summary_id: stored.id,
            text: item.text.clone(),
            owner: item.owner.clone(),
            due_at: None,
        })?;
    }

    Graph::new(&db).connect(
        NodeRef::new(NodeKind::Summary, stored.id),
        EdgeKind::DerivedFrom,
        NodeRef::new(NodeKind::Meeting, meeting_id),
    )?;

    Ok(Json(SummarizeResponse {
        summary_id: stored.id,
        text: summary.text,
        model: summary.model,
        decisions: decisions.len(),
        action_items: action_items.len(),
    }))
}

// ---------------------------------------------------------------- clarifying questions

#[derive(Debug, Deserialize)]
struct QuestionsBody {
    /// Meeting position to treat as "now", in milliseconds. Defaults to the last segment.
    #[serde(default)]
    now_ms: Option<i64>,
}

/// Suggest questions worth asking about a live meeting.
///
/// Stateless per request: the caller owns the cooldown and dedupe state, because a desktop
/// app and a browser tab watching the same meeting should not silence each other. The
/// windowing and gating logic is reused from `ClarifierSession` so both agree on policy.
async fn clarifying_questions(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<QuestionsBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let meeting_id = parse_id(&id)?;

    let (utterances, latest_ms) = {
        let db = state.db().await;
        let repo = MeetingRepository::new(&db);
        repo.get(meeting_id)?;

        let segments = repo.segments(meeting_id)?;
        let latest = segments.last().map(|s| s.end_ms).unwrap_or(0);

        let utterances: Vec<Utterance> = segments
            .into_iter()
            .map(|s| Utterance {
                speaker: s.speaker,
                text: s.text,
                at_ms: s.start_ms,
            })
            .collect();

        (utterances, latest)
    }; // lock released before the model call

    let now_ms = body.now_ms.unwrap_or(latest_ms);
    let session = ClarifierSession::new(ClarifierConfig::default());

    // Checked before spending a model call, not after.
    if !session.should_suggest(&utterances, now_ms) {
        return Ok(Json(serde_json::json!({
            "questions": [],
            "reason": "not enough recent transcript to ask about",
        })));
    }

    let window = session.window_text(&utterances, now_ms);
    let questions = suggest_questions(state.ai(), &window, now_ms).await?;

    Ok(Json(serde_json::json!({
        "questions": questions,
        "window_ms": session.config().window_ms,
    })))
}

// ---------------------------------------------------------------- chat

#[derive(Debug, Deserialize)]
struct ChatBody {
    messages: Vec<IncomingMessage>,
}

#[derive(Debug, Deserialize)]
struct IncomingMessage {
    role: String,
    content: String,
}

/// Ask a question about one meeting, grounded in its transcript.
async fn chat_about_meeting(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<ChatBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let meeting_id = parse_id(&id)?;

    if body.messages.is_empty() {
        return Err(ApiError::BadRequest("messages must not be empty".into()));
    }

    let (title, transcript) = {
        let db = state.db().await;
        let repo = MeetingRepository::new(&db);
        let meeting = repo.get(meeting_id)?;
        (meeting.title, repo.transcript_text(meeting_id)?)
    }; // lock released before the model call

    if transcript.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "this meeting has no transcript to ask about".into(),
        ));
    }

    let messages: Vec<ChatMessage> = body
        .messages
        .into_iter()
        .map(|m| ChatMessage {
            // Anything that is not explicitly the assistant is treated as the user; a
            // client-supplied role is not worth failing a request over.
            role: if m.role == "assistant" {
                Role::Assistant
            } else {
                Role::User
            },
            content: m.content,
        })
        .collect();

    let request = ChatRequest::new(messages)
        .with_context(vec![format!("Meeting: {title}\n\nTranscript:\n{transcript}")]);

    let response = state.ai().chat(&request).await?;

    Ok(Json(serde_json::json!({
        "text": response.text,
        "model": response.model,
    })))
}

// ---------------------------------------------------------------- backends

/// Every selectable AI backend, with what it needs and where it runs.
///
/// Locality comes from the kind rather than a live probe so a settings screen can show the
/// privacy implication of each option before the user commits to one.
async fn list_backends(State(state): State<Shared>) -> ApiResult<Json<serde_json::Value>> {
    let backends: Vec<_> = BackendKind::ALL
        .iter()
        .map(|kind| {
            serde_json::json!({
                "kind": kind.as_str(),
                "label": kind.label(),
                "is_local": kind.is_local(),
                "requires_api_key": kind.requires_api_key(),
                "requires_endpoint": kind.requires_endpoint(),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "backends": backends,
        "active": {
            "model": state.ai().model_id(),
            "is_local": state.ai().is_local(),
        },
    })))
}

// ---------------------------------------------------------------- models

/// Transcription models, with whether each is already downloaded.
///
/// Backs in-app model management, so a user never has to find a URL or a terminal.
async fn list_models(State(state): State<Shared>) -> ApiResult<Json<serde_json::Value>> {
    let store = model_store();
    let _ = &state; // model storage is independent of the database

    let models: Vec<_> = ModelRegistry::all()
        .into_iter()
        .map(|model| {
            serde_json::json!({
                "name": model.name,
                "size": model.size,
                "bytes": model.bytes,
                "approx_ram_mb": model.approx_ram_mb(),
                "multilingual": model.multilingual,
                "installed": store.is_available(&model),
                "recommended": model.name == ModelRegistry::default_model().name,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "models": models,
        "directory": store.dir().display().to_string(),
    })))
}

/// Download a model into the local store.
///
/// Runs to completion before responding. That is acceptable for a loopback call a user
/// explicitly started, but it means no progress reporting — a streaming variant is the
/// obvious next step for the larger models, which are gigabytes.
async fn download_model(Path(name): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    let model = ModelRegistry::get(&name)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let store = model_store();

    if store.is_available(&model) {
        return Ok(Json(serde_json::json!({
            "name": model.name,
            "installed": true,
            "already_present": true,
        })));
    }

    let path = store
        .download(&model)
        .await
        .map_err(|e| ApiError::BadRequest(format!("downloading {}: {e}", model.name)))?;

    Ok(Json(serde_json::json!({
        "name": model.name,
        "installed": true,
        "already_present": false,
        "path": path.display().to_string(),
    })))
}

/// Where models live.
///
/// Honours `NOTEWISE_MODEL_DIR`, then the platform data directory — the same resolution the
/// CLI uses, so both see one store.
fn model_store() -> ModelStore {
    if let Ok(dir) = std::env::var("NOTEWISE_MODEL_DIR") {
        return ModelStore::new(dir);
    }

    let base = std::env::var("NOTEWISE_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".into());
            if cfg!(target_os = "macos") {
                std::path::PathBuf::from(home).join("Library/Application Support/notewise")
            } else if cfg!(target_os = "windows") {
                std::path::PathBuf::from(home).join("AppData/Roaming/notewise")
            } else {
                std::path::PathBuf::from(home).join(".local/share/notewise")
            }
        });

    ModelStore::new(base.join("models"))
}

// ---------------------------------------------------------------- graph

#[derive(Debug, Deserialize)]
struct RelatedQuery {
    depth: Option<u32>,
}

#[derive(Debug, Serialize)]
struct RelatedNodeView {
    kind: NodeKind,
    id: Id,
    distance: u32,
    via: EdgeKind,
}

async fn related_to_meeting(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<RelatedQuery>,
) -> ApiResult<Json<Vec<RelatedNodeView>>> {
    let meeting_id = parse_id(&id)?;
    let db = state.db().await;
    MeetingRepository::new(&db).get(meeting_id)?;

    let related = Graph::new(&db).related(
        NodeRef::new(NodeKind::Meeting, meeting_id),
        query.depth.unwrap_or(2),
    )?;

    Ok(Json(
        related
            .into_iter()
            .map(|r| RelatedNodeView {
                kind: r.node.kind,
                id: r.node.id,
                distance: r.distance,
                via: r.via,
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------- export

#[derive(Debug, Deserialize)]
struct ExportQuery {
    /// `full` (default), `brief`, or `transcript`.
    #[serde(default)]
    variant: Option<String>,
}

/// Export a meeting as Markdown.
///
/// Returns `text/markdown` rather than JSON: the response is a document a user saves or
/// pastes, and wrapping it in a JSON envelope would make every client unwrap it again.
async fn export_meeting(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<ExportQuery>,
) -> ApiResult<axum::response::Response> {
    let meeting_id = parse_id(&id)?;

    let options = match query.variant.as_deref() {
        None | Some("full") => ExportOptions::default(),
        Some("brief") => ExportOptions::brief(),
        Some("transcript") => ExportOptions::transcript_only(),
        Some(other) => {
            return Err(ApiError::BadRequest(format!(
                "unknown export variant '{other}'; expected full, brief, or transcript"
            )))
        }
    };

    let db = state.db().await;
    let title = MeetingRepository::new(&db).get(meeting_id)?.title;
    let markdown = meeting_to_markdown(&db, meeting_id, options)?;

    // A filename the user recognizes, rather than a uuid.
    let filename = format!(
        "{}.md",
        title
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_lowercase()
    );

    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/markdown; charset=utf-8".to_string()),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        markdown,
    )
        .into_response())
}

// ---------------------------------------------------------------- notes & tickets

async fn list_notes(
    State(state): State<Shared>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<Note>>> {
    let db = state.db().await;
    Ok(Json(NoteRepository::new(&db).list_recent(query.limit())?))
}

#[derive(Debug, Deserialize)]
struct CreateNote {
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    project_id: Option<Id>,
    /// Meeting this note references, recorded as a graph edge.
    #[serde(default)]
    references_meeting: Option<Id>,
}

async fn create_note(
    State(state): State<Shared>,
    Json(body): Json<CreateNote>,
) -> ApiResult<Json<Note>> {
    let db = state.db().await;
    let note = NoteRepository::new(&db).create(NewNote {
        project_id: body.project_id,
        title: body.title,
        body: body.body,
    })?;

    if let Some(meeting_id) = body.references_meeting {
        Graph::new(&db).connect(
            NodeRef::new(NodeKind::Note, note.id),
            EdgeKind::References,
            NodeRef::new(NodeKind::Meeting, meeting_id),
        )?;
    }

    Ok(Json(note))
}

async fn list_tickets(State(state): State<Shared>) -> ApiResult<Json<Vec<Ticket>>> {
    let db = state.db().await;
    Ok(Json(TicketRepository::new(&db).list_open()?))
}

// ---------------------------------------------------------------- search

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
struct SearchHitView {
    kind: String,
    id: Id,
    title: String,
    snippet: String,
}

async fn search(
    State(state): State<Shared>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<Vec<SearchHitView>>> {
    let limit = query.limit.unwrap_or(25).clamp(1, 100);
    let db = state.db().await;

    Ok(Json(
        SearchRepository::new(&db)
            .search(&query.q, limit)?
            .into_iter()
            .map(|hit| SearchHitView {
                kind: hit.entity_kind,
                id: hit.entity_id,
                title: hit.title,
                snippet: hit.snippet,
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use notewise_ai_router::{Router as AiRouter, RouterConfig};
    use notewise_storage::Database;
    use tower::ServiceExt;

    fn app() -> AxumRouter {
        let state = AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        );
        router(Arc::new(state))
    }

    async fn call(app: &AxumRouter, request: Request<Body>) -> (StatusCode, serde_json::Value) {
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

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("request")
    }

    fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    }

    async fn create_test_meeting(app: &AxumRouter) -> Id {
        let (status, json) = call(app, post("/v1/meetings", serde_json::json!({"title": "Sync"}))).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        json["id"].as_str().unwrap().parse().unwrap()
    }

    #[tokio::test]
    async fn health_reports_schema_and_backend_locality() {
        let (status, json) = call(&app(), get("/health")).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "ok");
        assert_eq!(json["schema_version"], notewise_storage::SUPPORTED_VERSION);
        assert_eq!(json["ai_local"], true);
        assert_eq!(json["ai_model"], "mock");
    }

    #[tokio::test]
    async fn meetings_round_trip_through_the_api() {
        let app = app();
        let id = create_test_meeting(&app).await;

        let (status, json) = call(&app, get(&format!("/v1/meetings/{id}"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["title"], "Sync");
        assert!(json["ended_at"].is_null(), "a new meeting is still recording");

        let (status, list) = call(&app, get("/v1/meetings")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_unknown_meeting_is_404_not_500() {
        let unknown = Id::new();
        let (status, json) = call(&app(), get(&format!("/v1/meetings/{unknown}"))).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["code"], "not_found");
    }

    #[tokio::test]
    async fn a_malformed_id_is_400_not_500() {
        let (status, json) = call(&app(), get("/v1/meetings/not-a-uuid")).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["code"], "bad_request");
    }

    #[tokio::test]
    async fn an_unknown_meeting_source_is_rejected() {
        let (status, _) = call(
            &app(),
            post(
                "/v1/meetings",
                serde_json::json!({"title": "Sync", "source": "telepathy"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn ending_a_meeting_sets_its_end_time() {
        let app = app();
        let id = create_test_meeting(&app).await;

        let (status, json) = call(&app, post(&format!("/v1/meetings/{id}/end"), serde_json::json!({}))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!json["ended_at"].is_null());
    }

    #[tokio::test]
    async fn transcript_segments_append_and_read_back_in_order() {
        let app = app();
        let id = create_test_meeting(&app).await;

        let (status, json) = call(
            &app,
            post(
                &format!("/v1/meetings/{id}/transcript"),
                serde_json::json!([
                    {"text": "second", "start_ms": 2000, "end_ms": 3000},
                    {"text": "first", "start_ms": 0, "end_ms": 1000, "speaker": "Alex"},
                ]),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["appended"], 2);

        let (_, transcript) = call(&app, get(&format!("/v1/meetings/{id}/transcript"))).await;
        let segments = transcript.as_array().unwrap();
        assert_eq!(segments[0]["text"], "first");
        assert_eq!(segments[0]["speaker"], "Alex");
        assert_eq!(segments[1]["text"], "second");
    }

    #[tokio::test]
    async fn a_segment_ending_before_it_starts_is_rejected() {
        let app = app();
        let id = create_test_meeting(&app).await;

        let (status, _) = call(
            &app,
            post(
                &format!("/v1/meetings/{id}/transcript"),
                serde_json::json!([{"text": "backwards", "start_ms": 5000, "end_ms": 1000}]),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn summarizing_without_a_transcript_is_rejected() {
        let app = app();
        let id = create_test_meeting(&app).await;

        let (status, json) = call(
            &app,
            post(&format!("/v1/meetings/{id}/summarize"), serde_json::json!({})),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["error"].as_str().unwrap().contains("no transcript"));
    }

    #[tokio::test]
    async fn summarize_persists_the_summary_and_links_it_in_the_graph() {
        let app = app();
        let id = create_test_meeting(&app).await;

        call(
            &app,
            post(
                &format!("/v1/meetings/{id}/transcript"),
                serde_json::json!([{"text": "We agreed to ship Friday.", "start_ms": 0, "end_ms": 3000}]),
            ),
        )
        .await;

        let (status, json) = call(
            &app,
            post(&format!("/v1/meetings/{id}/summarize"), serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["model"], "mock");
        assert_eq!(json["decisions"], 1);
        assert_eq!(json["action_items"], 1);

        // The summary should now be reachable from the meeting by traversal.
        let (_, related) = call(&app, get(&format!("/v1/meetings/{id}/related"))).await;
        let summaries: Vec<_> = related
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| n["kind"] == "summary")
            .collect();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0]["distance"], 1);
        assert_eq!(summaries[0]["via"], "derived_from");
    }

    #[tokio::test]
    async fn a_note_referencing_a_meeting_becomes_reachable_from_it() {
        let app = app();
        let meeting_id = create_test_meeting(&app).await;

        let (status, _) = call(
            &app,
            post(
                "/v1/notes",
                serde_json::json!({
                    "title": "Follow-up",
                    "body": "Recap of the sync.",
                    "references_meeting": meeting_id.to_string(),
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (_, related) = call(&app, get(&format!("/v1/meetings/{meeting_id}/related"))).await;
        assert!(related
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "note"));
    }

    #[tokio::test]
    async fn excessive_traversal_depth_is_rejected() {
        let app = app();
        let id = create_test_meeting(&app).await;

        let (status, json) = call(&app, get(&format!("/v1/meetings/{id}/related?depth=99"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["code"], "depth_too_large");
    }

    #[tokio::test]
    async fn search_finds_a_created_note() {
        let app = app();
        call(
            &app,
            post(
                "/v1/notes",
                serde_json::json!({"title": "Migration plan", "body": "Move to Postgres."}),
            ),
        )
        .await;

        let (status, hits) = call(&app, get("/v1/search?q=Postgres")).await;
        assert_eq!(status, StatusCode::OK);
        let hits = hits.as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["kind"], "note");
    }

    #[tokio::test]
    async fn search_with_punctuation_does_not_error() {
        let (status, _) = call(&app(), get("/v1/search?q=%22unbalanced")).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn list_limits_are_clamped() {
        // A client asking for the entire history should not get it in one response.
        assert_eq!(ListQuery { limit: Some(10_000) }.limit(), 500);
        assert_eq!(ListQuery { limit: Some(0) }.limit(), 1);
        assert_eq!(ListQuery { limit: None }.limit(), 50);
    }

    async fn call_raw(app: &AxumRouter, request: Request<Body>) -> (StatusCode, String, String) {
        let response = app.clone().oneshot(request).await.expect("request");
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, content_type, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn export_returns_markdown_not_json() {
        let app = app();
        let id = create_test_meeting(&app).await;
        call(
            &app,
            post(
                &format!("/v1/meetings/{id}/transcript"),
                serde_json::json!([{"text": "We agreed to ship.", "start_ms": 0, "end_ms": 2000}]),
            ),
        )
        .await;

        let (status, content_type, body) =
            call_raw(&app, get(&format!("/v1/meetings/{id}/export"))).await;

        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("text/markdown"), "{content_type}");
        assert!(body.starts_with("# Sync"), "{body}");
        assert!(body.contains("## Transcript"), "{body}");
    }

    #[tokio::test]
    async fn export_variants_change_the_sections() {
        let app = app();
        let id = create_test_meeting(&app).await;
        call(
            &app,
            post(
                &format!("/v1/meetings/{id}/transcript"),
                serde_json::json!([{"text": "Something said.", "start_ms": 0, "end_ms": 2000}]),
            ),
        )
        .await;

        let (_, _, brief) =
            call_raw(&app, get(&format!("/v1/meetings/{id}/export?variant=brief"))).await;
        assert!(!brief.contains("## Transcript"), "{brief}");

        let (_, _, transcript) = call_raw(
            &app,
            get(&format!("/v1/meetings/{id}/export?variant=transcript")),
        )
        .await;
        assert!(transcript.contains("## Transcript"), "{transcript}");
        assert!(!transcript.contains("## Summary"), "{transcript}");
    }

    #[tokio::test]
    async fn an_unknown_export_variant_is_rejected() {
        let app = app();
        let id = create_test_meeting(&app).await;

        let (status, _, _) =
            call_raw(&app, get(&format!("/v1/meetings/{id}/export?variant=pdf"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn export_suggests_a_readable_filename() {
        let app = app();
        let id = create_test_meeting(&app).await;

        let response = app
            .clone()
            .oneshot(get(&format!("/v1/meetings/{id}/export")))
            .await
            .unwrap();
        let disposition = response
            .headers()
            .get("content-disposition")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        // "Sync" -> sync.md, not a uuid.
        assert!(disposition.contains("sync.md"), "{disposition}");
    }

    #[tokio::test]
    async fn questions_are_declined_when_there_is_too_little_transcript() {
        // Asking on the first sentence would make everything look ambiguous, and would
        // spend a model call to find that out.
        let app = app();
        let id = create_test_meeting(&app).await;
        call(
            &app,
            post(
                &format!("/v1/meetings/{id}/transcript"),
                serde_json::json!([{"text": "Morning.", "start_ms": 0, "end_ms": 1000}]),
            ),
        )
        .await;

        let (status, json) = call(
            &app,
            post(&format!("/v1/meetings/{id}/questions"), serde_json::json!({})),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["questions"].as_array().unwrap().is_empty());
        assert!(json["reason"].as_str().unwrap().contains("not enough"));
    }

    #[tokio::test]
    async fn questions_run_once_there_is_enough_transcript() {
        let app = app();
        let id = create_test_meeting(&app).await;
        call(
            &app,
            post(
                &format!("/v1/meetings/{id}/transcript"),
                serde_json::json!([{
                    "text": "We should move the database over before the launch because it                              will be much faster, and someone will need to handle the                              migration scripts and the index rebuild at some point soon.",
                    "start_ms": 0, "end_ms": 20000, "speaker": "Alex"
                }]),
            ),
        )
        .await;

        let (status, json) = call(
            &app,
            post(&format!("/v1/meetings/{id}/questions"), serde_json::json!({})),
        )
        .await;

        // The mock backend returns prose, which correctly parses to no questions — the
        // point is that the request was gated in and completed rather than refused.
        assert_eq!(status, StatusCode::OK, "{json}");
        assert!(json["questions"].is_array());
        assert!(json.get("reason").is_none(), "should not have been declined: {json}");
    }

    #[tokio::test]
    async fn questions_for_an_unknown_meeting_are_404() {
        let (status, _) = call(
            &app(),
            post(
                &format!("/v1/meetings/{}/questions", Id::new()),
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn chat_needs_a_transcript_to_ground_on() {
        let app = app();
        let id = create_test_meeting(&app).await;

        let (status, json) = call(
            &app,
            post(
                &format!("/v1/meetings/{id}/chat"),
                serde_json::json!({"messages": [{"role": "user", "content": "What happened?"}]}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["error"].as_str().unwrap().contains("no transcript"));
    }

    #[tokio::test]
    async fn chat_answers_against_the_transcript() {
        let app = app();
        let id = create_test_meeting(&app).await;
        call(
            &app,
            post(
                &format!("/v1/meetings/{id}/transcript"),
                serde_json::json!([{"text": "We agreed to ship Friday.", "start_ms": 0, "end_ms": 3000}]),
            ),
        )
        .await;

        let (status, json) = call(
            &app,
            post(
                &format!("/v1/meetings/{id}/chat"),
                serde_json::json!({"messages": [{"role": "user", "content": "When do we ship?"}]}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{json}");
        assert!(!json["text"].as_str().unwrap().is_empty());
        assert_eq!(json["model"], "mock");
    }

    #[tokio::test]
    async fn chat_rejects_an_empty_conversation() {
        let app = app();
        let id = create_test_meeting(&app).await;

        let (status, _) = call(
            &app,
            post(
                &format!("/v1/meetings/{id}/chat"),
                serde_json::json!({"messages": []}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn backends_are_listed_with_their_privacy_implication() {
        let (status, json) = call(&app(), get("/v1/backends")).await;

        assert_eq!(status, StatusCode::OK);
        let backends = json["backends"].as_array().unwrap();
        assert!(backends.len() >= 8);

        let ollama = backends.iter().find(|b| b["kind"] == "ollama").unwrap();
        assert_eq!(ollama["is_local"], true);
        assert_eq!(ollama["requires_api_key"], false);

        let anthropic = backends.iter().find(|b| b["kind"] == "anthropic").unwrap();
        assert_eq!(anthropic["is_local"], false);
        assert_eq!(anthropic["requires_api_key"], true);

        assert_eq!(json["active"]["model"], "mock");
        assert_eq!(json["active"]["is_local"], true);
    }

    #[tokio::test]
    async fn models_are_listed_with_install_state() {
        let (status, json) = call(&app(), get("/v1/models")).await;

        assert_eq!(status, StatusCode::OK);
        let models = json["models"].as_array().unwrap();
        assert!(models.len() >= 8);

        let recommended: Vec<_> = models.iter().filter(|m| m["recommended"] == true).collect();
        assert_eq!(recommended.len(), 1, "exactly one model should be recommended");
        assert_eq!(recommended[0]["name"], "base.en");

        // Everything a picker needs to warn before a user chooses badly.
        for model in models {
            assert!(model["bytes"].as_u64().unwrap() > 1_000_000);
            assert!(model["approx_ram_mb"].as_u64().unwrap() > 0);
            assert!(model["installed"].is_boolean());
        }
    }

    #[tokio::test]
    async fn downloading_an_unknown_model_is_rejected() {
        let (status, _) = call(
            &app(),
            post("/v1/models/gpt-9/download", serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn empty_ticket_list_is_returned_as_an_empty_array() {
        let (status, json) = call(&app(), get("/v1/tickets")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    // ------------------------------------------------------------------ recording

    /// `/health` must state whether capture is possible. Without this a client has to guess,
    /// and the only way to discover the truth is a record button that does nothing.
    #[tokio::test]
    async fn health_reports_whether_this_build_can_record() {
        let response = app()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let body: serde_json::Value = serde_json::from_slice(
            &response.into_body().collect().await.unwrap().to_bytes(),
        )
        .unwrap();

        assert!(body["can_record"].is_boolean(), "{body}");
        // This state is in-memory, so recording is impossible regardless of features.
        assert_eq!(body["can_record"], serde_json::Value::Bool(false));
        assert_eq!(body["recording_meeting_id"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn recording_status_is_idle_before_anything_starts() {
        let response = app()
            .oneshot(Request::get("/v1/recording").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &response.into_body().collect().await.unwrap().to_bytes(),
        )
        .unwrap();

        assert_eq!(body["recording"], serde_json::Value::Bool(false));
        assert_eq!(body["meeting_id"], serde_json::Value::Null);
    }

    /// Starting on an in-memory engine must fail loudly. A second connection to `:memory:` is
    /// a different, empty database, so "succeeding" would record into nothing.
    #[tokio::test]
    async fn starting_a_recording_on_an_ephemeral_engine_is_refused() {
        let response = app()
            .oneshot(
                Request::post("/v1/recording")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // 400 without a file-backed database, 501 in a build with no capture at all. Both are
        // honest refusals; neither is a 2xx and neither is a 500.
        assert!(
            matches!(
                response.status(),
                StatusCode::BAD_REQUEST | StatusCode::NOT_IMPLEMENTED
            ),
            "got {}",
            response.status()
        );

        let body: serde_json::Value = serde_json::from_slice(
            &response.into_body().collect().await.unwrap().to_bytes(),
        )
        .unwrap();
        // The message has to name the cause, since the fix differs completely between the two.
        let message = body["error"].as_str().unwrap_or_default();
        assert!(
            message.contains("in-memory") || message.contains("features"),
            "{message}"
        );
    }

    /// A stop with nothing running is a 409, not a silent success — a client that got 200 here
    /// would clear its recording UI while a real recording was still going.
    #[tokio::test]
    async fn stopping_when_nothing_is_recording_is_a_conflict() {
        let response = app()
            .oneshot(
                Request::delete("/v1/recording")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body: serde_json::Value = serde_json::from_slice(
            &response.into_body().collect().await.unwrap().to_bytes(),
        )
        .unwrap();
        assert_eq!(body["code"], "conflict");
    }

    /// The route must accept a bodyless POST, so `curl -X POST /v1/recording` works and a
    /// client is not forced to send `{}` to take the default device and model.
    #[tokio::test]
    async fn starting_a_recording_does_not_require_a_body() {
        let response = app()
            .oneshot(Request::post("/v1/recording").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_ne!(
            response.status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "a bodyless start should be accepted"
        );
        assert_ne!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
