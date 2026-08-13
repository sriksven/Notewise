//! HTTP route table and handlers.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router as AxumRouter};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use notewise_ai_router::{
    generate_email_variants, suggest_questions, AiBackend, BackendKind, ChatMessage, ChatRequest,
    ClarifierConfig, ClarifierSession, EmailContext, EmailTone, Role, TranscriptInput, Utterance,
};
use notewise_graph::{EdgeKind, Graph, NodeKind, NodeRef};
use notewise_storage::{
    meeting_to_markdown, DraftStatus, EmailDraft, EmailDraftRepository, ExportOptions, Id, Meeting,
    MeetingRepository, MeetingSource, NewEmailDraft, NewMeeting, NewNote, NewSummary,
    NewTranscriptSegment, Note, NoteRepository, SearchRepository, SummaryRepository, Ticket,
    TicketRepository, TranscriptSegment,
};
use notewise_transcription::{ModelRegistry, ModelStore};

use axum::response::sse::Event;
use futures_util::StreamExt;

use crate::downloads::{DownloadState, DownloadStatus};
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
        .route(
            "/v1/models/:name/download",
            post(download_model).get(download_progress),
        )
        .route("/v1/downloads", get(list_downloads))
        .route("/v1/devices", get(list_devices))
        .route("/v1/languages", get(list_languages))
        .route("/v1/backend", post(switch_backend))
        .route("/v1/import", post(import_audio))
        .route("/v1/search", get(search))
        // Drafting only. There is deliberately no route that sends one — see `draft_emails`.
        .route(
            "/v1/meetings/:id/emails",
            get(list_email_drafts).post(draft_emails),
        )
        .route("/v1/emails/:id/approve", post(approve_email_draft))
        .route("/v1/emails/:id", axum::routing::delete(discard_email_draft))
        .route(
            "/v1/recording",
            get(recording_status)
                .post(start_recording)
                .delete(stop_recording),
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

// ---------------------------------------------------------------- devices and languages

// Only constructed by the feature-gated device enumeration.
#[cfg_attr(not(feature = "record"), allow(dead_code))]
#[derive(Debug, Serialize)]
struct DeviceBody {
    name: String,
    is_default: bool,
    sample_rate: u32,
    channels: u16,
}

/// Input devices this machine can record from.
///
/// Returns an empty list rather than an error in a build without capture. A picker with nothing
/// in it is self-explanatory; a 501 makes a client decide whether to show an error for a
/// feature it may not even offer.
async fn list_devices() -> Json<serde_json::Value> {
    #[cfg(feature = "record")]
    match notewise_audio_capture::input_devices() {
        Ok(devices) => {
            let devices: Vec<DeviceBody> = devices
                .into_iter()
                .map(|d| DeviceBody {
                    name: d.name,
                    is_default: d.is_default,
                    sample_rate: d.sample_rate,
                    channels: d.channels,
                })
                .collect();
            Json(serde_json::json!({ "devices": devices, "available": true }))
        }
        Err(e) => {
            // Reported rather than swallowed: an empty picker and a broken audio subsystem
            // look identical to a user otherwise.
            tracing::warn!(error = %e, "could not enumerate input devices");
            Json(serde_json::json!({
                "devices": [],
                "available": true,
                "error": e.to_string(),
            }))
        }
    }

    #[cfg(not(feature = "record"))]
    Json(serde_json::json!({ "devices": [], "available": false }))
}

/// Languages Whisper can be told to expect.
///
/// A subset, not all ninety-nine: a picker with every language is harder to use than one with
/// the languages a meeting is plausibly in, and "Detect" covers the rest.
async fn list_languages() -> Json<serde_json::Value> {
    const LANGUAGES: [(&str, &str); 14] = [
        ("en", "English"),
        ("es", "Spanish"),
        ("fr", "French"),
        ("de", "German"),
        ("it", "Italian"),
        ("pt", "Portuguese"),
        ("nl", "Dutch"),
        ("hi", "Hindi"),
        ("ja", "Japanese"),
        ("ko", "Korean"),
        ("zh", "Chinese"),
        ("ru", "Russian"),
        ("ar", "Arabic"),
        ("tr", "Turkish"),
    ];

    Json(serde_json::json!({
        "languages": LANGUAGES
            .iter()
            .map(|(code, label)| serde_json::json!({ "code": code, "label": label }))
            .collect::<Vec<_>>(),
    }))
}

// ---------------------------------------------------------------- backend switching

#[derive(Debug, Deserialize)]
struct SwitchBackendBody {
    /// Backend identifier, e.g. `ollama` or `anthropic`.
    kind: String,
    /// Model id. Omit for the backend's default.
    model: Option<String>,
    /// Endpoint for backends that need one (LM Studio, a custom OpenAI-compatible server).
    endpoint: Option<String>,
}

/// Switch the active AI backend.
///
/// The API key is *not* accepted here. It is read from the environment, so a key never travels
/// over even a loopback HTTP request and never lands in a log or a shell history. A backend
/// whose key is missing is refused with a message naming the variable to set.
async fn switch_backend(
    State(state): State<Shared>,
    Json(body): Json<SwitchBackendBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let kind = BackendKind::parse(body.kind.trim())
        .ok_or_else(|| ApiError::BadRequest(format!("unknown backend '{}'", body.kind)))?;

    state
        .switch_backend(kind, body.model.clone(), body.endpoint.clone())
        .await?;

    let ai = state.ai();
    Ok(Json(serde_json::json!({
        "kind": kind.as_str(),
        "model": ai.model_id(),
        "is_local": ai.is_local(),
    })))
}

// ---------------------------------------------------------------- import

// Fields are read only by the feature-gated import path, but the shape must exist in every
// build so the route can return a clear 501 rather than a deserialization error.
#[cfg_attr(not(all(feature = "record", feature = "whisper")), allow(dead_code))]
#[derive(Debug, Deserialize)]
struct ImportBody {
    /// Absolute path to a WAV file on this machine.
    ///
    /// A path rather than an upload: the engine is loopback-only and the file is already on the
    /// same machine, so uploading would copy gigabytes through HTTP for no reason.
    path: String,
    title: Option<String>,
    model: Option<String>,
    language: Option<String>,
}

/// Transcribe an existing audio file into a new meeting.
///
/// Runs to completion before responding. Unlike a live recording there is no "stop", and a
/// caller has nothing useful to do with a half-imported meeting.
#[allow(unused_variables)]
async fn import_audio(
    State(state): State<Shared>,
    Json(body): Json<ImportBody>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    #[cfg(all(feature = "record", feature = "whisper"))]
    {
        let result = crate::recording::import_file(
            state.db_path().map(|p| p.to_path_buf()),
            state.model_dir().to_path_buf(),
            crate::recording::ImportRequest {
                path: std::path::PathBuf::from(&body.path),
                title: body.title.clone(),
                model: body.model.clone(),
                language: body.language.clone(),
            },
        )
        .await?;

        Ok((
            axum::http::StatusCode::CREATED,
            Json(serde_json::json!({
                "meeting_id": result.0,
                "segments": result.1.segments,
                "speakers": result.1.speakers,
                "audio_ms": result.1.audio_ms,
            })),
        ))
    }
    #[cfg(not(all(feature = "record", feature = "whisper")))]
    Err(ApiError::NotImplemented(
        "this build cannot transcribe: it was compiled without the 'record' and 'whisper' \
         features"
            .into(),
    ))
}

// ---------------------------------------------------------------- email drafts

/// # There is no send endpoint, and that is the design
///
/// This surface can create, list, approve, and discard drafts. It cannot send one, and nothing
/// else in this repository can either. A wrong auto-send is the highest-consequence failure
/// this product has: it reaches other people, it cannot be recalled, and the user finds out
/// from the recipient.
///
/// `POST /v1/emails/:id/sent` is also absent even though the storage layer has `mark_sent`.
/// Exposing it would let a client record a message as sent when nothing sent it, which is worse
/// than not tracking the state at all — it would put a lie in the user's audit trail.
#[derive(Debug, Deserialize)]
struct DraftEmailsBody {
    /// Which tones to draft. Defaults to a single concise draft.
    tones: Option<Vec<String>>,
    /// Who the mail is for, e.g. "the platform team".
    audience: Option<String>,
    /// Who is sending, so the model does not invent a sign-off.
    sender: Option<String>,
    /// Pre-filled recipients. Never resolved from the transcript: addresses the user did not
    /// type are exactly the ones that end up wrong.
    recipients: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct EmailDraftBody {
    id: Id,
    meeting_id: Option<Id>,
    subject: String,
    body: String,
    recipients: Vec<String>,
    status: String,
    variant: Option<String>,
    created_at: DateTime<Utc>,
}

impl From<EmailDraft> for EmailDraftBody {
    fn from(draft: EmailDraft) -> Self {
        Self {
            id: draft.id,
            meeting_id: draft.meeting_id,
            subject: draft.subject,
            body: draft.body,
            recipients: draft.recipients,
            status: match draft.status {
                DraftStatus::Draft => "draft",
                DraftStatus::Approved => "approved",
                DraftStatus::Sent => "sent",
                DraftStatus::Discarded => "discarded",
            }
            .to_string(),
            variant: draft.variant,
            created_at: draft.created_at,
        }
    }
}

/// Draft one or more follow-up emails for a meeting.
///
/// Prefers the meeting's summary as source material: it drafts better than a raw transcript and
/// costs a fraction of the tokens. Falls back to the transcript when nothing has been
/// summarised yet.
async fn draft_emails(
    State(state): State<Shared>,
    Path(id): Path<String>,
    body: Option<Json<DraftEmailsBody>>,
) -> ApiResult<(axum::http::StatusCode, Json<Vec<EmailDraftBody>>)> {
    let meeting_id = parse_id(&id)?;
    let body = body.map(|Json(b)| b).unwrap_or(DraftEmailsBody {
        tones: None,
        audience: None,
        sender: None,
        recipients: None,
    });

    let tones = parse_tones(body.tones.as_deref())?;

    // Everything the model needs, gathered under one short lock and released before the call.
    let context = {
        let db = state.db().await;
        let meetings = MeetingRepository::new(&db);
        let meeting = meetings.get(meeting_id)?;

        let summaries = SummaryRepository::new(&db);
        let summary = summaries.latest_for_meeting(meeting_id)?;

        let (source, decisions, action_items) = match &summary {
            Some(summary) => (
                summary.text.clone(),
                summaries
                    .decisions(summary.id)?
                    .into_iter()
                    .map(|d| d.text)
                    .collect(),
                summaries
                    .action_items(summary.id)?
                    .into_iter()
                    .map(|a| (a.text, a.owner))
                    .collect(),
            ),
            None => (
                meetings.transcript_text(meeting_id)?,
                Vec::new(),
                Vec::new(),
            ),
        };

        let mut context = EmailContext::new(meeting.title, source)
            .with_decisions(decisions)
            .with_action_items(action_items);
        if let Some(sender) = body.sender.clone() {
            context = context.with_sender(sender);
        }
        if let Some(audience) = body.audience.clone() {
            context = context.with_audience(audience);
        }
        context
    };

    let ai = state.ai();
    let drafts = generate_email_variants(&*ai, &context, &tones).await?;

    let db = state.db().await;
    let repo = EmailDraftRepository::new(&db);
    let mut stored = Vec::with_capacity(drafts.len());

    for draft in drafts {
        // Created in `Draft`. The repository has no method that produces any other state.
        stored.push(
            repo.create(NewEmailDraft {
                meeting_id: Some(meeting_id),
                subject: draft.subject,
                body: draft.body,
                recipients: body.recipients.clone().unwrap_or_default(),
                variant: Some(draft.tone.as_str().to_string()),
            })?
            .into(),
        );
    }

    Ok((axum::http::StatusCode::CREATED, Json(stored)))
}

async fn list_email_drafts(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<EmailDraftBody>>> {
    let meeting_id = parse_id(&id)?;
    let db = state.db().await;
    let drafts = EmailDraftRepository::new(&db).list_for_meeting(meeting_id)?;
    Ok(Json(drafts.into_iter().map(Into::into).collect()))
}

/// Mark a draft approved.
///
/// Approval is not sending. It records that a human read this text and considers it correct —
/// the prerequisite the state machine enforces before anything could ever send it.
async fn approve_email_draft(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<EmailDraftBody>> {
    let draft_id = parse_id(&id)?;
    let db = state.db().await;
    Ok(Json(
        EmailDraftRepository::new(&db).approve(draft_id)?.into(),
    ))
}

async fn discard_email_draft(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<EmailDraftBody>> {
    let draft_id = parse_id(&id)?;
    let db = state.db().await;
    Ok(Json(
        EmailDraftRepository::new(&db).discard(draft_id)?.into(),
    ))
}

/// Resolve tone names, rejecting unknown ones rather than silently substituting a default.
///
/// A user who asked for "formal" and got a chatty draft would have no way to tell the name was
/// ignored, and might send it.
fn parse_tones(names: Option<&[String]>) -> ApiResult<Vec<EmailTone>> {
    let Some(names) = names else {
        return Ok(vec![EmailTone::Concise]);
    };

    if names.is_empty() {
        return Ok(vec![EmailTone::Concise]);
    }

    names
        .iter()
        .map(|name| {
            EmailTone::parse(name).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "unknown tone '{name}' — expected one of: {}",
                    EmailTone::ALL
                        .iter()
                        .map(|t| t.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })
        })
        .collect()
}

// ---------------------------------------------------------------- recording

#[derive(Debug, Deserialize)]
struct StartRecordingBody {
    title: Option<String>,
    /// Input device name. Omit for the system default.
    device: Option<String>,
    /// Transcription model, e.g. `base.en`. Omit for the default.
    model: Option<String>,
    /// Spoken language, e.g. `en`. Omit to let the model detect it.
    language: Option<String>,
    /// Separate speakers when the recording stops. Defaults to on.
    diarize: Option<bool>,
}

#[derive(Debug, Serialize)]
struct RecordingStatusBody {
    recording: bool,
    meeting_id: Option<Id>,
    device: Option<String>,
    model: Option<String>,
    language: Option<String>,
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
        language: status.as_ref().and_then(|s| s.language.clone()),
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
                language: body.as_ref().and_then(|b| b.language.clone()),
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
            language: status.language,
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
            // A path that does not exist is the caller's error, so 400 rather than 500.
            RecordingError::NoSuchFile(_) => ApiError::BadRequest(error.to_string()),
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
    let questions = suggest_questions(&*state.ai(), &window, now_ms).await?;

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

    let request = ChatRequest::new(messages).with_context(vec![format!(
        "Meeting: {title}\n\nTranscript:\n{transcript}"
    )]);

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
/// Start downloading a model.
///
/// Returns immediately with the download's initial state rather than holding the connection
/// open. `large-v3` is 3.1 GB: a blocking request would be killed by a sleeping laptop or an
/// impatient proxy, and a retry would start a second transfer of the same file. Progress comes
/// from `GET /v1/models/:name/download`.
async fn download_model(
    State(state): State<Shared>,
    Path(name): Path<String>,
) -> ApiResult<(axum::http::StatusCode, Json<DownloadState>)> {
    let model = ModelRegistry::get(&name).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let store = state.model_store();

    if store.is_available(&model) {
        // 200 rather than 202: nothing was accepted for later, it is already here.
        return Ok((
            axum::http::StatusCode::OK,
            Json(DownloadState::already_installed(&model)),
        ));
    }

    let started = state.downloads().start(model, store).await;
    Ok((axum::http::StatusCode::ACCEPTED, Json(started)))
}

/// Stream a download's progress as Server-Sent Events.
///
/// SSE rather than WebSockets: this is one-way, it is a plain `GET` that survives proxies, and
/// the browser reconnects on its own. A polling client would either miss the end of a fast
/// download or hammer the engine through a slow one.
///
/// The stream closes on the terminal event, so a client knows the download is over without
/// having to time anything out.
async fn download_progress(
    State(state): State<Shared>,
    Path(name): Path<String>,
) -> ApiResult<
    axum::response::Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>>,
> {
    let model = ModelRegistry::get(&name).map_err(|e| ApiError::BadRequest(e.to_string()))?;

    let receiver = match state.downloads().subscribe(&model.name).await {
        Some(receiver) => receiver,
        None if state.model_store().is_available(&model) => {
            // Already installed and never downloaded by this process. Emit one terminal event
            // rather than 404 — the client's question is "is it here yet", and the answer is yes.
            let (sender, receiver) =
                tokio::sync::watch::channel(DownloadState::already_installed(&model));
            // Keep the sender alive for the life of the receiver.
            std::mem::forget(sender);
            receiver
        }
        None => {
            return Err(ApiError::NotFound(format!(
                "no download in progress for '{}'",
                model.name
            )))
        }
    };

    let stream = tokio_stream::wrappers::WatchStream::new(receiver)
        // `take_while` keeps the terminal event and then ends: inclusive, because a client that
        // never saw `done` would sit on a stalled progress bar forever.
        .scan(false, |ended, state| {
            if *ended {
                return std::future::ready(None);
            }
            *ended = state.status.is_terminal();
            std::future::ready(Some(state))
        })
        .map(|state| {
            Ok(Event::default()
                .event(match state.status {
                    DownloadStatus::Downloading => "progress",
                    DownloadStatus::Done => "done",
                    DownloadStatus::Failed => "failed",
                })
                .json_data(state)
                .unwrap_or_else(|_| Event::default().data("{}")))
        });

    Ok(axum::response::Sse::new(stream).keep_alive(
        // A 3 GB download over slow wifi can go a long time between megabyte reports, and an
        // idle proxy will close a connection it thinks is dead.
        axum::response::sse::KeepAlive::new().interval(std::time::Duration::from_secs(15)),
    ))
}

/// Every download this engine has started, running or finished.
async fn list_downloads(State(state): State<Shared>) -> Json<Vec<DownloadState>> {
    Json(state.downloads().all().await)
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
            .map(|c| if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            })
            .collect::<String>()
            .trim_matches('-')
            .to_lowercase()
    );

    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/markdown; charset=utf-8".to_string(),
            ),
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
        let (status, json) = call(
            app,
            post("/v1/meetings", serde_json::json!({"title": "Sync"})),
        )
        .await;
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
        assert!(
            json["ended_at"].is_null(),
            "a new meeting is still recording"
        );

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

        let (status, json) = call(
            &app,
            post(&format!("/v1/meetings/{id}/end"), serde_json::json!({})),
        )
        .await;
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
            post(
                &format!("/v1/meetings/{id}/summarize"),
                serde_json::json!({}),
            ),
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
            post(
                &format!("/v1/meetings/{id}/summarize"),
                serde_json::json!({}),
            ),
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
        assert_eq!(
            ListQuery {
                limit: Some(10_000)
            }
            .limit(),
            500
        );
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
        (
            status,
            content_type,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
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

        let (_, _, brief) = call_raw(
            &app,
            get(&format!("/v1/meetings/{id}/export?variant=brief")),
        )
        .await;
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
            post(
                &format!("/v1/meetings/{id}/questions"),
                serde_json::json!({}),
            ),
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
            post(
                &format!("/v1/meetings/{id}/questions"),
                serde_json::json!({}),
            ),
        )
        .await;

        // The mock backend returns prose, which correctly parses to no questions — the
        // point is that the request was gated in and completed rather than refused.
        assert_eq!(status, StatusCode::OK, "{json}");
        assert!(json["questions"].is_array());
        assert!(
            json.get("reason").is_none(),
            "should not have been declined: {json}"
        );
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
        assert_eq!(
            recommended.len(),
            1,
            "exactly one model should be recommended"
        );
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

        let body: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
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
        let body: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
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

        let body: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
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
        let body: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
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

    // ------------------------------------------------------------------ email drafts

    async fn meeting_with_transcript(app: &AxumRouter) -> String {
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/meetings")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"title":"Infra sync","source":"combined"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let meeting: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let id = meeting["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(
                Request::post(format!("/v1/meetings/{id}/transcript"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"[{"text":"We agreed to move off SQLite before the launch.",
                             "start_ms":0,"end_ms":4000,"speaker":"Alex"}]"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        id
    }

    async fn json(app: &AxumRouter, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    }

    #[tokio::test]
    async fn drafting_produces_one_draft_per_tone_all_in_draft_state() {
        let app = app();
        let id = meeting_with_transcript(&app).await;

        let (status, body) = json(
            &app,
            Request::post(format!("/v1/meetings/{id}/emails"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"tones":["concise","formal"],"sender":"Alex"}"#,
                ))
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED, "{body}");
        let drafts = body.as_array().expect("array");
        assert_eq!(drafts.len(), 2, "{body}");

        for draft in drafts {
            // Nothing may be created in any state but Draft.
            assert_eq!(draft["status"], "draft", "{draft}");
            assert!(!draft["subject"].as_str().unwrap().is_empty());
            assert!(!draft["body"].as_str().unwrap().is_empty());
        }
        assert_eq!(drafts[0]["variant"], "concise");
        assert_eq!(drafts[1]["variant"], "formal");
    }

    #[tokio::test]
    async fn drafting_defaults_to_a_single_concise_draft() {
        let app = app();
        let id = meeting_with_transcript(&app).await;

        let (status, body) = json(
            &app,
            Request::post(format!("/v1/meetings/{id}/emails"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(body[0]["variant"], "concise");
    }

    /// An unknown tone must be rejected, not silently replaced. A user who asked for "formal"
    /// and received a chatty draft has no way to notice before sending it.
    #[tokio::test]
    async fn an_unknown_tone_is_rejected_rather_than_defaulted() {
        let app = app();
        let id = meeting_with_transcript(&app).await;

        let (status, body) = json(
            &app,
            Request::post(format!("/v1/meetings/{id}/emails"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"tones":["shouty"]}"#))
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = body["error"].as_str().unwrap_or_default();
        assert!(message.contains("shouty"), "{message}");
        assert!(
            message.contains("concise"),
            "should list valid tones: {message}"
        );
    }

    #[tokio::test]
    async fn drafts_are_listed_for_their_meeting() {
        let app = app();
        let id = meeting_with_transcript(&app).await;

        app.clone()
            .oneshot(
                Request::post(format!("/v1/meetings/{id}/emails"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let (status, body) = json(
            &app,
            Request::get(format!("/v1/meetings/{id}/emails"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_draft_can_be_approved_and_discarded() {
        let app = app();
        let id = meeting_with_transcript(&app).await;

        let (_, created) = json(
            &app,
            Request::post(format!("/v1/meetings/{id}/emails"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let draft_id = created[0]["id"].as_str().unwrap().to_string();

        let (status, approved) = json(
            &app,
            Request::post(format!("/v1/emails/{draft_id}/approve"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{approved}");
        assert_eq!(approved["status"], "approved");

        let (status, discarded) = json(
            &app,
            Request::delete(format!("/v1/emails/{draft_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{discarded}");
        assert_eq!(discarded["status"], "discarded");
    }

    /// **The load-bearing test for this feature.**
    ///
    /// No route may send an email or mark one sent. A wrong auto-send reaches other people and
    /// cannot be recalled, so the absence of the capability is enforced here rather than left
    /// to reviewer memory. If a send route is ever added deliberately, this test is the place
    /// that has to be argued with first.
    #[tokio::test]
    async fn no_route_can_send_an_email_or_mark_one_sent() {
        let app = app();
        let id = meeting_with_transcript(&app).await;

        let (_, created) = json(
            &app,
            Request::post(format!("/v1/meetings/{id}/emails"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let draft_id = created[0]["id"].as_str().unwrap().to_string();

        for path in [
            format!("/v1/emails/{draft_id}/send"),
            format!("/v1/emails/{draft_id}/sent"),
            format!("/v1/emails/{draft_id}/deliver"),
            format!("/v1/meetings/{id}/emails/send"),
            "/v1/emails/send".to_string(),
        ] {
            for method in ["POST", "PUT", "PATCH"] {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(&path)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();

                assert!(
                    matches!(
                        response.status(),
                        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
                    ),
                    "{method} {path} answered {} — a send path must not exist",
                    response.status()
                );
            }
        }

        // And the draft is still a draft.
        let (_, listed) = json(
            &app,
            Request::get(format!("/v1/meetings/{id}/emails"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(listed[0]["status"], "draft");
    }

    /// Recipients come from the caller. Addresses inferred from a transcript are exactly the
    /// ones that turn out to be wrong.
    #[tokio::test]
    async fn recipients_are_only_ever_what_the_caller_supplied() {
        let app = app();
        let id = meeting_with_transcript(&app).await;

        let (_, empty) = json(
            &app,
            Request::post(format!("/v1/meetings/{id}/emails"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(empty[0]["recipients"].as_array().unwrap().len(), 0);

        let (_, given) = json(
            &app,
            Request::post(format!("/v1/meetings/{id}/emails"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"recipients":["sam@example.com"]}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(given[0]["recipients"][0], "sam@example.com");
    }

    #[tokio::test]
    async fn drafting_a_meeting_with_no_content_is_refused() {
        let app = app();
        let (_, meeting) = json(
            &app,
            Request::post("/v1/meetings")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"title":"Empty","source":"combined"}"#))
                .unwrap(),
        )
        .await;
        let id = meeting["id"].as_str().unwrap();

        let (status, _) = json(
            &app,
            Request::post(format!("/v1/meetings/{id}/emails"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ------------------------------------------------------------------ downloads

    #[tokio::test]
    async fn no_downloads_are_reported_before_any_start() {
        let (status, body) = json(
            &app(),
            Request::get("/v1/downloads").body(Body::empty()).unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn an_unknown_model_cannot_be_downloaded_or_watched() {
        let app = app();

        let (status, body) = json(
            &app,
            Request::post("/v1/models/not-a-model/download")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        let (status, _) = json(
            &app,
            Request::get("/v1/models/not-a-model/download")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// Watching a download nobody started is a 404, not an empty stream a client would sit on
    /// forever waiting for a `done` that is never coming.
    #[tokio::test]
    async fn watching_a_download_that_was_never_started_is_a_404() {
        // `large-v3` is real but certainly not installed in a test environment.
        let (status, body) = json(
            &app(),
            Request::get("/v1/models/large-v3/download")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "not_found");
    }

    /// The SSE route must be a real event stream, not JSON. A client using `EventSource`
    /// fails silently on the wrong content type.
    #[tokio::test]
    async fn an_installed_model_streams_a_terminal_event_rather_than_erroring() {
        let dir = tempfile::tempdir().expect("tempdir");
        let model = notewise_transcription::ModelRegistry::get("tiny.en").expect("tiny.en");
        std::fs::write(
            dir.path().join(format!("ggml-{}.bin", model.name)),
            vec![0u8; model.bytes as usize],
        )
        .expect("fake model");

        let state = AppState::new(
            Database::open_in_memory().expect("db"),
            AiRouter::from_config(RouterConfig::mock()).expect("router"),
        )
        .with_model_dir(dir.path());

        let response = router(Arc::new(state))
            .oneshot(
                Request::get("/v1/models/tiny.en/download")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream"),
            "EventSource fails silently on anything else"
        );
    }

    /// Downloading something already on disk is a 200, not a 202: nothing was accepted for
    /// later, and a client waiting for a stream would wait forever.
    #[tokio::test]
    async fn downloading_an_installed_model_reports_done_immediately() {
        let dir = tempfile::tempdir().expect("tempdir");
        let model = notewise_transcription::ModelRegistry::get("tiny.en").expect("tiny.en");
        std::fs::write(
            dir.path().join(format!("ggml-{}.bin", model.name)),
            vec![0u8; model.bytes as usize],
        )
        .expect("fake model");

        let state = AppState::new(
            Database::open_in_memory().expect("db"),
            AiRouter::from_config(RouterConfig::mock()).expect("router"),
        )
        .with_model_dir(dir.path());

        let response = router(Arc::new(state))
            .oneshot(
                Request::post("/v1/models/tiny.en/download")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["status"], "done");
        assert_eq!(body["percent"], 100);
    }
}
