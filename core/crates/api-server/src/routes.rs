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
    ClarifierConfig, ClarifierSession, EmailContext, EmailTone, Role, Router as AiRouter,
    RouterConfig, TranscriptInput, Utterance,
};
use notewise_graph::{EdgeKind, Graph, NodeKind, NodeRef};
use notewise_storage::{
    meeting_to_markdown, DraftStatus, EmailDraft, EmailDraftRepository, ExportOptions, Id, Meeting,
    MeetingRepository, MeetingSource, NewEmailDraft, NewMeeting, NewNote, NewSummary,
    NewTranscriptSegment, Note, NoteRepository, SearchRepository, SettingsRepository,
    SummaryRepository, Ticket, TicketRepository, TranscriptSegment,
};
use notewise_transcription::ModelRegistry;

use axum::response::sse::Event;
use futures_util::StreamExt;
// Only used by the feature-gated upload path.
#[cfg(all(feature = "record", feature = "whisper"))]
use tokio::io::AsyncWriteExt;

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
        // Identity only, never audio. The engine already has the audio; what it cannot know is
        // which of four remote voices is which. See `crate::speakers`.
        .route(
            "/v1/meetings/:id/speaker-events",
            post(crate::speakers::post_speaker_events),
        )
        // Naming a voice by hand. The floor under every automatic approach: clustering can
        // prove there were three voices and can never learn one of them was Priya.
        .route(
            "/v1/meetings/:id/speakers",
            get(crate::speakers::list_speakers),
        )
        .route(
            "/v1/meetings/:id/speakers/rename",
            post(crate::speakers::rename_speaker),
        )
        // Acoustic separation: the setting, and the model it needs. Off by default — see
        // `crate::diarization` for why a guess is not the default answer to "who spoke".
        .route(
            "/v1/diarization",
            get(crate::diarization::get_status).put(crate::diarization::update_status),
        )
        .route("/v1/speaker-models", get(crate::diarization::list_models))
        .route(
            "/v1/speaker-models/:name/download",
            post(crate::diarization::download_model),
        )
        .route(
            "/v1/speaker-models/:name",
            axum::routing::delete(crate::diarization::remove_model),
        )
        .route("/v1/meetings/:id/summarize", post(summarize_meeting))
        .route("/v1/meetings/:id/summary", get(get_summary))
        .route("/v1/meetings/:id/related", get(related_to_meeting))
        .route("/v1/meetings/:id/export", get(export_meeting))
        .route("/v1/notes", get(list_notes).post(create_note))
        .route("/v1/tickets", get(list_tickets))
        .route("/v1/meetings/:id/questions", post(clarifying_questions))
        .route("/v1/meetings/:id/chat", post(chat_about_meeting))
        .route("/v1/backends", get(list_backends))
        .route("/v1/backends/:kind/models", get(list_backend_models))
        .route(
            "/v1/backends/:kind/key",
            post(set_api_key).delete(delete_api_key),
        )
        .route(
            "/v1/preferences",
            get(get_preferences).post(set_preferences),
        )
        .route("/v1/models", get(list_models))
        .route(
            "/v1/models/:name/download",
            post(download_model).get(download_progress),
        )
        .route("/v1/downloads", get(list_downloads))
        .route("/v1/setup", get(setup_readiness))
        .route("/v1/setup/complete", post(complete_setup))
        .route("/v1/permissions/:kind", post(request_permission))
        .route("/v1/devices", get(list_devices))
        .route("/v1/languages", get(list_languages))
        .route("/v1/backend", post(switch_backend))
        .merge(crate::routing::routes())
        .merge(crate::jobs::routes())
        .merge(crate::join::routes())
        // External tools: server configuration, proposals, and the confirmation that runs one.
        .merge(crate::tools::routes())
        // Dictation, and what the desktop assistant can do on this machine.
        .merge(crate::dictation::routes())
        // Asking about the screen, acting on a selection, continuing a sentence.
        .merge(crate::assistant::routes())
        // Extraction settings and a manual run. The memory CRUD lives in `routing`.
        .merge(crate::memory::routes())
        // Mirroring a meeting to a vault, and settling a file the user edited.
        .merge(crate::vault::routes())
        .route("/v1/import", post(import_audio))
        .route(
            "/v1/import/upload",
            // axum caps bodies at 2 MB by default, which no recording clears. The body is
            // streamed straight to disk, so the ceiling bounds the file rather than memory.
            post(import_upload).layer(axum::extract::DefaultBodyLimit::max(4 << 30)),
        )
        .route("/v1/search", get(search))
        // Drafting only. There is deliberately no route that sends one — see `draft_emails`.
        .route(
            "/v1/meetings/:id/emails",
            get(list_email_drafts).post(draft_emails),
        )
        .route("/v1/emails/:id/approve", post(approve_email_draft))
        // The route out for somebody with no mailbox connected, which is most people trying this.
        .route("/v1/emails/:id/eml", get(download_draft_as_eml))
        .route("/v1/emails/:id", axum::routing::delete(discard_email_draft))
        .route(
            "/v1/recording",
            get(recording_status)
                .post(start_recording)
                .delete(stop_recording),
        )
        // Connector status and configuration.
        .route("/v1/connectors", get(crate::connectors::list_connectors))
        // Registered before `:id`, for the same reason as `failures` below.
        .route(
            "/v1/connectors/available",
            get(crate::connectors::list_available_connectors),
        )
        // Registered *before* `:id`, or axum matches the literal `failures` against the
        // parameter and this endpoint disappears behind a connector named "failures".
        .route(
            "/v1/connectors/failures",
            get(crate::connectors::list_failed_deliveries),
        )
        // Registered *before* `:id` for the same reason `failures` is: axum would otherwise match
        // the literal segment against the parameter.
        .route(
            "/v1/connectors/microsoft/signin",
            post(crate::connectors::start_microsoft_signin)
                .get(crate::connectors::microsoft_signin_status),
        )
        .route("/v1/connectors/sync", post(crate::connectors::sync_now))
        .route(
            "/v1/connectors/:id",
            post(crate::connectors::connect_connector)
                .delete(crate::connectors::disconnect_connector),
        )
        // Workspace writes — notes, tickets, action items, decisions, people, series.
        // Kept in their own module so this table stays readable.
        .merge(crate::workspace::router())
        // Grounded question answering over a note or the whole workspace.
        .merge(crate::ask::router())
        // The agent: multi-step research across the workspace, ending in a note.
        .merge(crate::agent::router())
        // Building and inspecting the semantic index.
        .merge(crate::indexing::router())
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
    /// The engine's own version, from its crate metadata.
    ///
    /// Reported by the engine rather than read from the frontend's `package.json`: the two are
    /// separately versioned and can be running from different builds, and the one that matters
    /// when something misbehaves is whichever is actually serving the request.
    version: &'static str,
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
        version: env!("CARGO_PKG_VERSION"),
        schema_version,
        ai_local: state.ai().is_local(),
        // The policy-aware label, not `model_id()`. A user with routing configured is not using
        // one model, and this field is read by a human. `model_id()` stays the default's model
        // because it is what gets persisted and read back to build a backend.
        ai_model: state.ai().model_label(),
        // Recording also needs a file-backed database, so an `--ephemeral` engine correctly
        // reports that it cannot record even in a build that otherwise could.
        can_record: recording::SUPPORTED && state.db_path().is_some(),
        recording_meeting_id: state.recording().status().await.map(|s| s.meeting_id),
    }))
}

/// The stored summary for a meeting, with its decisions and action items.
///
/// Read-only and separate from `POST /summarize`: a summary is generated once and looked at
/// many times, and re-running a model every time someone opens a meeting would be both slow
/// and non-deterministic.
async fn get_summary(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let meeting_id = parse_id(&id)?;
    let db = state.db().await;
    let repo = SummaryRepository::new(&db);

    // 200 with `summary: null` rather than 404: the meeting exists, it simply has not been
    // summarised, and a client should render "not summarised yet" rather than an error.
    let Some(summary) = repo.latest_for_meeting(meeting_id)? else {
        return Ok(Json(serde_json::json!({ "summary": null })));
    };

    let decisions = repo.decisions(summary.id)?;
    let action_items = repo.action_items(summary.id)?;

    Ok(Json(serde_json::json!({
        "summary": {
            "id": summary.id,
            "text": summary.text,
            "model": summary.model,
            "created_at": summary.created_at,
            "decisions": decisions
                .into_iter()
                .map(|d| serde_json::json!({
                    "id": d.id,
                    "text": d.text,
                    "reasoning": d.reasoning,
                }))
                .collect::<Vec<_>>(),
            "action_items": action_items
                .into_iter()
                .map(|a| serde_json::json!({
                    "id": a.id,
                    "text": a.text,
                    "owner": a.owner,
                    "due_at": a.due_at,
                }))
                .collect::<Vec<_>>(),
        }
    })))
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
#[cfg(feature = "record")]
async fn list_devices() -> Json<serde_json::Value> {
    use notewise_audio_capture::{CaptureKind, PermissionStatus};

    // Asked before enumerating, because enumerating without the grant does not fail — it
    // *hangs*. CoreAudio waits on a TCC decision that never arrives, the request never
    // answers, and the picker sits empty forever showing "no devices found" while the real
    // answer is "macOS will not tell you until you allow the microphone".
    let permission = tokio::task::spawn_blocking(|| {
        notewise_audio_capture::permission_status(CaptureKind::Microphone)
    })
    .await
    .unwrap_or(PermissionStatus::NotRequested);

    if !matches!(permission, PermissionStatus::Granted) {
        return Json(serde_json::json!({
            "devices": [],
            "available": true,
            "error": "macOS does not list input devices until microphone access is allowed",
        }));
    }

    // On a blocking thread even so. This is a synchronous CoreAudio call that can take
    // hundreds of milliseconds with a Bluetooth device waking up, and holding a runtime
    // worker for that stalls every other request.
    //
    // Bounded as well, because "granted" is what TCC believes and a wedged audio daemon is
    // still possible. A picker that says it could not read the list is recoverable; a
    // request that never returns is not.
    let enumerated = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::task::spawn_blocking(notewise_audio_capture::input_devices),
    )
    .await;

    match enumerated {
        Ok(Ok(Ok(devices))) => {
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
        // Reported rather than swallowed: an empty picker and a broken audio subsystem
        // look identical to a user otherwise.
        Ok(Ok(Err(e))) => {
            tracing::warn!(error = %e, "could not enumerate input devices");
            Json(serde_json::json!({
                "devices": [], "available": true, "error": e.to_string(),
            }))
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "the device enumeration thread failed");
            Json(serde_json::json!({
                "devices": [], "available": true, "error": "the audio system did not respond",
            }))
        }
        Err(_) => {
            tracing::warn!("timed out enumerating input devices");
            Json(serde_json::json!({
                "devices": [],
                "available": true,
                "error": "the audio system did not answer in time",
            }))
        }
    }
}

/// Without capture compiled in there is nothing to enumerate, and saying so beats an empty
/// list that looks like a machine with no microphone.
#[cfg(not(feature = "record"))]
async fn list_devices() -> Json<serde_json::Value> {
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

    // The mock backend is reachable by starting the engine with `NOTEWISE_BACKEND=mock`, which
    // is a deliberate act. It is not reachable from a running app, where the result would be
    // fabricated summaries of a real meeting that look exactly like real ones.
    if !kind.is_selectable() {
        return Err(ApiError::BadRequest(format!(
            "'{}' cannot be selected at runtime — it does not run a model and would return \
             invented answers",
            kind.as_str()
        )));
    }

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

/// The key under which the window's own settings live.
const PREFERENCES_KEY: &str = "ui_preferences";

/// Read the interface preferences.
///
/// These live in the engine rather than in `localStorage`, which is not an available option:
/// the desktop shell binds port 0, so the window's origin changes on every launch and anything
/// kept per-origin is gone by the next one. A theme that resets every time you open the app is
/// not a theme.
async fn get_preferences(State(state): State<Shared>) -> ApiResult<Json<serde_json::Value>> {
    let db = state.db().await;
    let stored = SettingsRepository::new(&db).get(PREFERENCES_KEY)?;

    Ok(Json(match stored {
        Some(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({})),
        None => serde_json::json!({}),
    }))
}

/// Replace the interface preferences.
///
/// Stored opaquely. What a theme consists of is the window's business, and a schema here would
/// have to be migrated every time the interface grew a setting.
async fn set_preferences(
    State(state): State<Shared>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    if !body.is_object() {
        return Err(ApiError::BadRequest("preferences must be an object".into()));
    }

    let db = state.db().await;
    SettingsRepository::new(&db).set(PREFERENCES_KEY, &body.to_string())?;
    Ok(Json(body))
}

#[derive(Debug, Deserialize)]
struct ApiKeyBody {
    key: String,
}

/// Save a provider API key.
///
/// The key goes to the OS keychain, not the database and not a log. The database is a plain
/// SQLite file that ends up in backups and support bundles; the keychain is the one place on
/// each platform designed to hold this.
///
/// This is a deliberate relaxation of an earlier rule that keys could only come from the
/// engine's environment. That rule protected against a key crossing HTTP — but it also meant
/// the only way to use a provider was to edit a shell profile and restart, which is not a thing
/// a desktop app can ask of someone. The request is loopback-only, the value is never written to
/// a log, and it is never readable back through the API.
async fn set_api_key(
    State(state): State<Shared>,
    Path(kind): Path<String>,
    Json(body): Json<ApiKeyBody>,
) -> ApiResult<Json<serde_json::Value>> {
    use notewise_connectors::{CredentialStore, KeychainStore, Secret};

    let kind = BackendKind::parse(kind.trim())
        .ok_or_else(|| ApiError::BadRequest(format!("unknown backend '{kind}'")))?;

    if !kind.requires_api_key() {
        return Err(ApiError::BadRequest(format!(
            "{} does not use an API key",
            kind.label()
        )));
    }

    let key = body.key.trim();
    if key.is_empty() {
        return Err(ApiError::BadRequest("the key must not be empty".into()));
    }

    KeychainStore::new()
        .set(
            &crate::state::key_entry(kind),
            crate::state::API_KEY_FIELD,
            &Secret::new(key),
        )
        .map_err(|e| ApiError::Internal(format!("could not save the key: {e}")))?;

    // Switch to it immediately. Saving a key and then having to find the provider in a menu is
    // two steps for one intention, and the failure mode of the second being forgotten is a user
    // who believes their key does not work.
    state.switch_backend(kind, None, None).await.map_err(|e| {
        ApiError::BadRequest(format!("the key was saved, but {kind:?} rejected it: {e}"))
    })?;

    tracing::info!(backend = kind.as_str(), "api key saved to the keychain");
    Ok(Json(serde_json::json!({
        "kind": kind.as_str(),
        "has_key": true,
        "model": state.ai().model_id(),
    })))
}

/// Forget a saved key. Removing one that is not there succeeds.
async fn delete_api_key(Path(kind): Path<String>) -> ApiResult<axum::http::StatusCode> {
    use notewise_connectors::{CredentialStore, KeychainStore};

    let kind = BackendKind::parse(kind.trim())
        .ok_or_else(|| ApiError::BadRequest(format!("unknown backend '{kind}'")))?;

    KeychainStore::new()
        .delete(&crate::state::key_entry(kind), crate::state::API_KEY_FIELD)
        .map_err(|e| ApiError::Internal(format!("could not remove the key: {e}")))?;

    tracing::info!(backend = kind.as_str(), "api key removed");
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// What a local backend can actually run.
///
/// Exists because a default model id is a guess. The engine ships pointing at `llama3.1`; a
/// machine with `llama3.1:8b` pulled answers every request with a 404, and until now there was
/// no way to correct that from inside the app — the backend picker chose a *provider*, never a
/// model. Asking the daemon is the only way to know the exact tags.
///
/// Never fails the request. A stopped daemon is a normal state, not an error, and the picker
/// that calls this should say "not running" rather than raise a banner over the app.
async fn list_backend_models(Path(kind): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    let kind = BackendKind::parse(kind.trim())
        .ok_or_else(|| ApiError::BadRequest(format!("unknown backend '{kind}'")))?;

    if !kind.lists_models() {
        return Ok(Json(serde_json::json!({
            "models": [],
            "available": false,
            "reason": format!("{} does not publish an installed-model list", kind.label()),
        })));
    }

    // Built for the question and dropped. Listing what Ollama holds must not require switching
    // the running backend to it first — that would mean breaking summarization to find out
    // whether a model exists.
    let probe = match AiRouter::from_config(RouterConfig::new(kind)) {
        Ok(router) => router,
        Err(e) => {
            return Ok(Json(serde_json::json!({
                "models": [],
                "available": false,
                "reason": e.to_string(),
            })))
        }
    };

    match probe.installed_models().await {
        Ok(models) => Ok(Json(serde_json::json!({
            "models": models,
            "available": true,
            "reason": serde_json::Value::Null,
        }))),
        Err(e) => Ok(Json(serde_json::json!({
            "models": [],
            "available": false,
            "reason": e.to_string(),
        }))),
    }
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

/// Decode a percent-encoded header value.
///
/// Written here rather than taken as a dependency: this decodes one header, and the failure mode
/// of a malformed escape is to keep the literal characters, which is what a file name containing
/// a stray `%` should do anyway.
#[cfg_attr(
    not(all(feature = "record", feature = "whisper")),
    allow(dead_code, reason = "only the upload path decodes a file name")
)]
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(byte) = hex {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }

    // Lossy rather than a rejection: a name that is not valid UTF-8 becomes a title with a
    // replacement character in it, which beats refusing to import the audio.
    String::from_utf8_lossy(&out).into_owned()
}

/// Transcribe an uploaded file.
///
/// The path endpoint above exists because the engine and the file are on the same machine, and
/// for the CLI that is exactly right. A window cannot use it: a browser file picker hands over
/// bytes and deliberately never reveals where they came from, so the only way to offer a real
/// "choose a file" button without a native dialog and the IPC that comes with it is to accept
/// the bytes.
///
/// The copy costs less than it sounds. This is loopback, and the alternative was a text box
/// asking the user to type an absolute path from memory.
///
/// The body is the raw file. Not multipart: there is exactly one part, and hand-rolling a
/// multipart parser to carry a filename that arrives in a header anyway would be work with no
/// return.
async fn import_upload(
    State(state): State<Shared>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    #[cfg(all(feature = "record", feature = "whisper"))]
    {
        // The name is for the meeting's title and the temp file's extension only; it never
        // becomes a path. A caller sending `../../etc/passwd` gets a meeting with a silly name.
        let supplied = headers
            .get("x-notewise-filename")
            .and_then(|v| v.to_str().ok())
            // Percent-encoded by the client, because a header may only carry ASCII and a file
            // name may not — "Réunion.wav" would otherwise arrive mangled or be dropped.
            .map(percent_decode)
            .map(|n| n.rsplit(['/', '\\']).next().unwrap_or(&n).to_string())
            .filter(|n| !n.trim().is_empty());

        let extension = supplied
            .as_deref()
            .and_then(|n| n.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()))
            .filter(|ext| ext.chars().all(|c| c.is_ascii_alphanumeric()) && ext.len() <= 8)
            .unwrap_or_else(|| "wav".to_string());

        // Written to a temp file the OS will clean up, then transcribed from disk — the decoder
        // reads a file, and buffering a gigabyte of audio in memory to avoid one local write
        // would be the worse trade.
        let dir = std::env::temp_dir().join("notewise-imports");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| ApiError::Internal(format!("could not prepare the import folder: {e}")))?;

        // Streamed to disk rather than collected first. An hour of 48 kHz stereo is well over
        // a gigabyte, and buffering that in memory to write it out again would make importing a
        // real meeting recording a way to exhaust the machine.
        let path = dir.join(format!("{}.{extension}", Id::new()));
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| ApiError::Internal(format!("could not stage the upload: {e}")))?;

        let mut stream = body.into_data_stream();
        let mut written: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ApiError::BadRequest(format!("upload failed: {e}")))?;
            written += chunk.len() as u64;
            if let Err(e) = file.write_all(&chunk).await {
                // Half a file on disk is worse than none: the decoder would read it as a
                // truncated recording and produce a transcript that silently stops early.
                let _ = tokio::fs::remove_file(&path).await;
                return Err(ApiError::Internal(format!(
                    "could not write the upload: {e}"
                )));
            }
        }
        file.flush()
            .await
            .map_err(|e| ApiError::Internal(format!("could not finish the upload: {e}")))?;
        drop(file);

        if written == 0 {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(ApiError::BadRequest("the upload is empty".into()));
        }

        let title = supplied.as_deref().map(|n| {
            n.rsplit_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(n)
                .to_string()
        });

        let result = crate::recording::import_file(
            state.db_path().map(|p| p.to_path_buf()),
            state.model_dir().to_path_buf(),
            crate::recording::ImportRequest {
                path: path.clone(),
                title,
                model: None,
                language: headers
                    .get("x-notewise-language")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            },
        )
        .await;

        // Removed whether or not the import worked. A failed transcription must not leave the
        // user's audio sitting in a temp folder they will never look in.
        let _ = tokio::fs::remove_file(&path).await;
        let result = result?;

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
    {
        let _ = (state, headers, body);
        Err(ApiError::NotImplemented(
            "this build cannot transcribe: it was compiled without the 'record' and 'whisper' \
             features"
                .into(),
        ))
    }
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
/// Approve a draft, and put it in the user's mailbox if a vendor with mail access is connected.
///
/// # What approving does and does not mean
///
/// It moves the draft to `Approved` and creates a *provider-side draft*. It does not send anything,
/// and `mark_sent` is not called here — creating a Gmail draft is not sending, and recording it as
/// sent would corrupt the one state machine the email module was built around. The user opens the
/// draft in Gmail or Outlook and presses send themselves.
///
/// # Why the mailbox hop is best effort
///
/// The approval is the user's decision and it has already been recorded. A vendor being unreachable,
/// or not connected, or connected without mail access, must not undo it — the draft is still in
/// Notewise and still readable. So a failure is reported in the response rather than returned as an
/// error.
async fn approve_email_draft(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<EmailDraftBody>> {
    let draft_id = parse_id(&id)?;

    let draft = {
        let db = state.db().await;
        EmailDraftRepository::new(&db).approve(draft_id)?
    };

    if let Err(e) = crate::connectors::enqueue_mail_draft(&state, &draft).await {
        // Logged rather than returned: see above. The interface shows the draft either way, and the
        // outbox row — when one was made — carries its own failure for the connectors screen.
        tracing::info!(error = %e, "the draft was approved but not put in a mailbox");
    }

    Ok(Json(draft.into()))
}

/// A draft as a file any mail client can open.
///
/// The path for a user with neither Google nor Microsoft connected — which is most people, and all of
/// them on a first run. Opening the file puts the draft in front of them with recipients and subject
/// filled in, which is the same end state the connectors reach by a route that needs no vendor, no
/// token, and no review.
///
/// Served as a download rather than as JSON the frontend assembles: the format has rules about line
/// endings and header escaping that belong in one place, and `storage::export` is where the other
/// renderer already lives.
async fn download_draft_as_eml(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<axum::response::Response> {
    let draft_id = parse_id(&id)?;

    let draft = {
        let db = state.db().await;
        EmailDraftRepository::new(&db).get(draft_id)?
    };

    let file_name = eml_file_name(&draft.subject);
    let body = notewise_storage::draft_to_eml(&draft);

    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "message/rfc822".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file_name}\""),
            ),
        ],
        body,
    )
        .into_response())
}

/// A filename from a subject.
///
/// Every character a filesystem or a `Content-Disposition` header could object to becomes a dash,
/// including the quote that would end the header's own quoted string.
fn eml_file_name(subject: &str) -> String {
    let stem: String = subject
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();

    let trimmed = stem.trim_matches(['-', ' ']).trim();
    let stem = if trimmed.is_empty() { "draft" } else { trimmed };

    // Bounded: a subject is model-generated and a two-hundred-character filename is refused by some
    // filesystems outright.
    format!("{}.eml", stem.chars().take(80).collect::<String>())
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
    /// Also capture system audio, as a second channel. Defaults to on.
    ///
    /// On a call this separates you from everyone else exactly, with no guessing. Where the
    /// platform will not provide it — no Screen Recording grant, or not macOS — the recording
    /// quietly falls back to the microphone alone.
    capture_system_audio: Option<bool>,
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
    /// Segments given a real name from platform speaker events, or `null` when none were posted.
    ///
    /// Distinguishes "the extension was connected and named 40 segments" from "no identity source
    /// was present", which a UI needs in order to explain anonymous labels rather than look broken.
    named_from_platform: Option<usize>,
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
                capture_system_audio: body
                    .as_ref()
                    .and_then(|b| b.capture_system_audio)
                    .unwrap_or(true),
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

    // After the pipeline has flushed, so the segments to be named all exist.
    let named = name_speakers(&state, meeting_id).await;

    Ok(Json(StoppedBody {
        meeting_id,
        segments: outcome.segments,
        // Naming splits one channel into several people, so the count the pipeline reported is
        // stale as soon as any segment gains a name.
        speakers: match named {
            Some(_) => distinct_speakers(&state, meeting_id)
                .await
                .unwrap_or(outcome.speakers),
            None => outcome.speakers,
        },
        audio_ms: outcome.audio_ms,
        named_from_platform: named,
    }))
}

/// How many distinct speakers a meeting's stored segments name.
async fn distinct_speakers(state: &Shared, meeting_id: Id) -> ApiResult<usize> {
    let db = state.db().await;
    let speakers: std::collections::HashSet<String> = MeetingRepository::new(&db)
        .segments(meeting_id)?
        .into_iter()
        .filter_map(|s| s.speaker)
        .collect();
    Ok(speakers.len())
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

    let ended = {
        let db = state.db().await;
        MeetingRepository::new(&db).end(id, Utc::now())?
    };

    // The meeting is over, so no further speaker events are coming and the timeline is as
    // complete as it will get. Done after the lock is released: naming takes the lock itself.
    name_speakers(&state, id).await;

    Ok(Json(ended))
}

/// Apply any accumulated speaker events to a meeting that has just ended.
///
/// Both end paths call this — a recording stopping and a meeting being closed directly — because
/// events can be posted for either, including for a meeting this engine never recorded.
async fn name_speakers(state: &Shared, meeting_id: Id) -> Option<usize> {
    // Drained before the database lock is taken, never while holding it: the guard is not `Sync`,
    // so awaiting with it alive would make this handler's future non-`Send`.
    let pending = crate::speakers::take_pending(state.speaker_timelines(), meeting_id).await?;

    let db = state.db().await;
    crate::speakers::apply_timeline(&db, meeting_id, pending)
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

    // During a live recording this fires several times a second and the debounce swallows all
    // of it — the pass runs once the transcript stops growing, which is when the meeting is over
    // and there is something worth indexing.
    drop(db);
    crate::indexing::touch(Arc::clone(&state));

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

#[derive(Debug, Deserialize)]
struct SummarizeParams {
    /// Which summary template's prompt to use. Omitted means the backend's own instruction.
    template: Option<String>,
}

/// Summarize a meeting, persist the results, and wire them into the graph.
///
/// The database lock is deliberately released before the model call — summarization can take
/// tens of seconds, and holding the lock across it would stall every other request.
async fn summarize_meeting(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Query(params): Query<SummarizeParams>,
) -> ApiResult<Json<SummarizeResponse>> {
    let meeting_id = parse_id(&id)?;

    // A query parameter rather than a body, so every existing caller keeps working and
    // "summarize with the default prompt" stays a bare POST.
    let template = match params.template.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => {
            let template_id = parse_id(raw)?;
            let db = state.db().await;
            Some(SummaryRepository::new(&db).template(template_id)?)
        }
    };

    let (title, transcript, project_id) = {
        let db = state.db().await;
        let repo = MeetingRepository::new(&db);
        let meeting = repo.get(meeting_id)?;
        (
            meeting.title,
            repo.transcript_text(meeting_id)?,
            meeting.project_id,
        )
    }; // lock released here, before any model call

    if transcript.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "meeting has no transcript to summarize".into(),
        ));
    }

    // What the workspace knows about the person, so a summary is written for them rather than for
    // nobody. Ranked against the meeting's title, which is the only query this call has.
    let memories = crate::memory::for_prompt(&state, project_id, &title).await;

    let mut input = TranscriptInput::new(title, transcript);

    // Memories go in front of the template rather than replacing it: a template is an instruction
    // about shape and a memory is context about the reader, and the two are not competing.
    let instructions = match (memories.is_empty(), &template) {
        (true, None) => None,
        (true, Some(template)) => Some(template.prompt.clone()),
        (false, None) => Some(memories.clone()),
        (false, Some(template)) => Some(format!("{memories}\n{}", template.prompt)),
    };
    if let Some(instructions) = instructions {
        input = input.with_instructions(instructions);
    }
    // Only the summary honours the template. Decisions and action items are extractions with a
    // fixed output shape, and a prompt written to change prose would break the parse.
    let summary = state.ai().summarize(&input).await?;
    let decisions = state.ai().extract_decisions(&input).await?;
    let action_items = state.ai().extract_action_items(&input).await?;

    let db = state.db().await;
    let repo = SummaryRepository::new(&db);
    let stored = repo.create(NewSummary {
        meeting_id,
        text: summary.text.clone(),
        model: summary.model.clone(),
        template_id: template.as_ref().map(|t| t.id),
    })?;

    for decision in &decisions {
        repo.add_decision(notewise_storage::NewDecision {
            meeting_id,
            summary_id: Some(stored.id),
            text: decision.text.clone(),
            reasoning: decision.reasoning.clone(),
            decided_at: None,
        })?;
    }
    for item in &action_items {
        repo.add_action_item(notewise_storage::NewActionItem {
            meeting_id,
            summary_id: Some(stored.id),
            text: item.text.clone(),
            owner: item.owner.clone(),
            owner_person_id: None,
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

    let (title, transcript, project_id) = {
        let db = state.db().await;
        let repo = MeetingRepository::new(&db);
        let meeting = repo.get(meeting_id)?;
        (
            meeting.title,
            repo.transcript_text(meeting_id)?,
            meeting.project_id,
        )
    }; // lock released before the model call

    if transcript.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "this meeting has no transcript to ask about".into(),
        ));
    }

    // Ranked against what was actually asked, which is the case cosine ordering exists for.
    let asked = body
        .messages
        .iter()
        .rev()
        .find(|m| m.role != "assistant")
        .map(|m| m.content.clone())
        .unwrap_or_default();
    let memories = crate::memory::for_prompt(&state, project_id, &asked).await;

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

    // The memory block first, so the transcript is the last thing the model reads.
    let mut context = Vec::new();
    if !memories.is_empty() {
        context.push(memories);
    }
    context.push(format!("Meeting: {title}\n\nTranscript:\n{transcript}"));

    let request = ChatRequest::new(messages).with_context(context);

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
        // Mock is excluded. It answers every request with fixed text, so a user who picked it
        // from a menu would get summaries and answers that were never derived from their
        // meeting, presented exactly like real ones.
        .filter(|kind| kind.is_selectable())
        .map(|kind| {
            serde_json::json!({
                "kind": kind.as_str(),
                "label": kind.label(),
                "is_local": kind.is_local(),
                "requires_api_key": kind.requires_api_key(),
                "requires_endpoint": kind.requires_endpoint(),
                "lists_models": kind.lists_models(),
                // Whether a key is available, from the keychain or the environment. The key
                // itself is never sent — only the fact that there is one, which is what the
                // UI needs to tell "add a key" apart from "ready to use".
                "has_key": !kind.requires_api_key() || crate::state::api_key_for(*kind).is_some(),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "backends": backends,
        "active": {
            // `kind` so a client can match the active backend against the list above. Without
            // it the only way to name what is running is to guess from `is_local`, which
            // matches several entries and picks the wrong one.
            "kind": state.ai().kind().as_str(),
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
    // `state.model_store()`, not the environment-derived `model_store()`: the desktop shell
    // configures a directory the free function does not know about, and a listing that
    // disagrees with the downloader reports installed models as missing.
    let store = state.model_store();

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
                // What the name means. `tiny.en` versus `medium` is not a choice anyone can
                // make from a size in megabytes.
                "tradeoff": model.size.tradeoff(),
                "language_note": model.language_note(),
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

// ---------------------------------------------------------------- setup

/// What first-run setup still needs.
///
/// Never prompts. Permission status is read without opening a device, so loading the wizard
/// cannot raise an OS dialog before the user has pressed anything.
async fn setup_readiness(
    State(state): State<Shared>,
) -> ApiResult<Json<crate::setup::SetupReadiness>> {
    Ok(Json(readiness(&state).await?))
}

#[derive(Debug, Default, Deserialize)]
struct CompleteSetupQuery {
    /// Finish with required steps still unsatisfied.
    ///
    /// Has to be asked for explicitly. Without it an accidental call is still refused, which is
    /// the whole point of checking server-side — but a user who cannot satisfy a step is not
    /// thereby locked out of their own machine.
    #[serde(default)]
    skip: bool,
}

/// Mark first-run setup finished.
///
/// Re-checks readiness server-side, because a gate enforced only in the UI is not a gate. The
/// check refuses an *unintended* completion; it is not a capability lock. A denied microphone is
/// not always something the user can reverse — it may belong to an administrator, or they may
/// only want to import files today — and refusing to open the app over it would strand them with
/// no way forward. `?skip=true` records that, and the answer names what was left unresolved.
async fn complete_setup(
    State(state): State<Shared>,
    Query(params): Query<CompleteSetupQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let readiness = readiness(&state).await?;

    // Already finished: answer with the original timestamp. Rewriting it would make a
    // double-click look like a second setup, and re-checking readiness could refuse someone
    // who legitimately completed setup before a model was later removed.
    if let Some(existing) = readiness.completed_at {
        return Ok(Json(serde_json::json!({ "completed_at": existing })));
    }

    let unsatisfied = readiness.unsatisfied();
    if !unsatisfied.is_empty() && !params.skip {
        return Err(ApiError::Conflict(format!(
            "setup is not finished: {} still {} attention",
            unsatisfied.join(", "),
            if unsatisfied.len() == 1 {
                "needs"
            } else {
                "need"
            }
        )));
    }

    let stamp = Utc::now().to_rfc3339();
    {
        let db = state.db().await;
        SettingsRepository::new(&db).set(crate::setup::COMPLETED_KEY, &stamp)?;
    }

    if unsatisfied.is_empty() {
        tracing::info!("first-run setup completed");
    } else {
        tracing::warn!(skipped = ?unsatisfied, "first-run setup completed with steps skipped");
    }

    // The skipped steps are reported back rather than swallowed: a client that let someone
    // through needs to be able to say what will not work, and `GET /v1/setup` keeps answering
    // truthfully afterwards.
    Ok(Json(
        serde_json::json!({ "completed_at": stamp, "skipped": unsatisfied }),
    ))
}

/// Ask the OS for a capability, prompting if it decides to.
///
/// Runs on a blocking thread: opening an audio device is not async, and holding a runtime
/// worker while a modal permission dialog waits on the user would stall every other request.
async fn request_permission(
    Path(kind): Path<String>,
) -> ApiResult<Json<crate::setup::PermissionReadiness>> {
    let kind = CaptureKindArg::parse(&kind).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "unknown permission '{kind}' — expected 'microphone' or 'system_audio'"
        ))
    })?;

    let readiness = tokio::task::spawn_blocking(move || permission_readiness(kind, true))
        .await
        .map_err(|e| ApiError::Internal(format!("the permission probe panicked: {e}")))?;

    Ok(Json(readiness))
}

/// What a refusal means, when "you declined this" is not the whole story.
///
/// Screen recording has a failure that looks identical to a decline and is not one: macOS will
/// not add an app to the Screen & System Audio Recording list unless it carries a distributable
/// signature. `CGRequestScreenCaptureAccess` then returns false immediately, no dialog appears,
/// and nothing is written to TCC — so "grant it in System Settings" points at a list the app is
/// missing from, with no way to put it there.
///
/// Both outcomes are covered rather than guessed between, because the two are indistinguishable
/// from inside the process. Telling the user what to look for lets them tell the difference in
/// one glance, which is more use than a confident wrong diagnosis.
#[cfg(feature = "record")]
fn denial_detail(kind: CaptureKindArg) -> Option<String> {
    if !matches!(kind, CaptureKindArg::SystemAudio) || !cfg!(target_os = "macos") {
        return None;
    }

    Some(
        "Look under Privacy & Security → Screen & System Audio Recording. If Notewise is \
         listed, switch it on and press Re-check. If it is not listed, macOS refused to \
         register this build — that needs a signed release build, and no setting will add it."
            .into(),
    )
}

/// Assemble the readiness snapshot both setup routes work from.
async fn readiness(state: &AppState) -> ApiResult<crate::setup::SetupReadiness> {
    use crate::setup::{PermissionsReadiness, SetupReadiness, StepReadiness, Steps};

    let completed_at = {
        let db = state.db().await;
        SettingsRepository::new(&db).get(crate::setup::COMPLETED_KEY)?
    };

    let model_installed = !state.model_store().installed().is_empty();
    let backend_reachable = state.ai().probe().await.is_ok();

    Ok(SetupReadiness {
        completed_at,
        steps: Steps {
            model: StepReadiness {
                satisfied: model_installed,
                required: true,
            },
            backend: StepReadiness {
                satisfied: backend_reachable,
                required: true,
            },
            permissions: PermissionsReadiness::from_parts(
                permission_readiness(CaptureKindArg::Microphone, false),
                permission_readiness(CaptureKindArg::SystemAudio, false),
            ),
        },
    })
}

/// Which capability a permission route is about.
///
/// A local enum rather than re-exporting `CaptureKind`, because `audio-capture` is an
/// optional dependency and this route table must compile without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureKindArg {
    Microphone,
    SystemAudio,
}

impl CaptureKindArg {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "microphone" => Some(Self::Microphone),
            "system_audio" => Some(Self::SystemAudio),
            _ => None,
        }
    }
}

/// Report one capability. `prompt` opens a device and may raise an OS dialog.
fn permission_readiness(kind: CaptureKindArg, prompt: bool) -> crate::setup::PermissionReadiness {
    #[cfg(feature = "record")]
    {
        use notewise_audio_capture::{CaptureKind, PermissionStatus};

        let requested = kind;
        let kind = match kind {
            CaptureKindArg::Microphone => CaptureKind::Microphone,
            CaptureKindArg::SystemAudio => CaptureKind::SystemAudio,
        };

        let probed = if prompt {
            notewise_audio_capture::request_permission(kind)
        } else {
            notewise_audio_capture::permission_status(kind)
        };

        let (status, detail) = match probed {
            PermissionStatus::NotRequested => ("not_requested", None),
            PermissionStatus::Granted => ("granted", None),
            PermissionStatus::Denied => ("denied", denial_detail(requested)),
            PermissionStatus::Unavailable(reason) => ("unavailable", Some(reason)),
        };

        crate::setup::PermissionReadiness {
            status: status.into(),
            // Only an obtainable permission gates the user. Anything unavailable has no action
            // behind it, so requiring it would be a trap.
            required: status != "unavailable",
            detail,
        }
    }

    #[cfg(not(feature = "record"))]
    {
        let _ = (kind, prompt);
        crate::setup::PermissionReadiness {
            status: "unavailable".into(),
            required: false,
            detail: Some(
                "this build has no capture support (built without the 'record' feature)".into(),
            ),
        }
    }
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

    // Released before `touch`, which takes the lock itself.
    drop(db);
    crate::indexing::touch(Arc::clone(&state));

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
    /// The meeting a transcript hit was said in, so a result can be opened.
    ///
    /// Null for kinds that do not belong to a meeting. Without it a search for a phrase someone
    /// remembers hearing returns the id of a transcript row, which no screen can show — the
    /// index knew the answer and the API threw it away.
    meeting_id: Option<Id>,
}

async fn search(
    State(state): State<Shared>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<Vec<SearchHitView>>> {
    let limit = query.limit.unwrap_or(25).clamp(1, 100);
    let db = state.db().await;

    let hits = SearchRepository::new(&db).search(&query.q, limit)?;

    let segment_ids: Vec<Id> = hits
        .iter()
        .filter(|hit| hit.entity_kind == "transcript_segment")
        .map(|hit| hit.entity_id)
        .collect();

    let owners: std::collections::HashMap<Id, Id> = MeetingRepository::new(&db)
        .segment_meetings(&segment_ids)?
        .into_iter()
        .collect();

    // A transcript segment has no title of its own, so it borrows the meeting's — a result
    // reading "Postgres migration sync" is something a user can recognise; a blank one is not.
    let titles: std::collections::HashMap<Id, String> = {
        let meetings = MeetingRepository::new(&db);
        let mut titles = std::collections::HashMap::new();
        for meeting_id in owners.values() {
            if let std::collections::hash_map::Entry::Vacant(slot) = titles.entry(*meeting_id) {
                if let Ok(meeting) = meetings.get(*meeting_id) {
                    slot.insert(meeting.title);
                }
            }
        }
        titles
    };

    Ok(Json(
        hits.into_iter()
            .map(|hit| {
                let meeting_id = owners.get(&hit.entity_id).copied();
                let title = match (&hit.title, meeting_id) {
                    (title, Some(meeting)) if title.is_empty() => {
                        titles.get(&meeting).cloned().unwrap_or_default()
                    }
                    (title, _) => title.clone(),
                };

                SearchHitView {
                    kind: hit.entity_kind,
                    id: hit.entity_id,
                    title,
                    snippet: hit.snippet,
                    meeting_id,
                }
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

    // ------------------------------------------------------------ speaker events

    /// Store transcript segments already labelled with a channel, as channel recording does.
    async fn add_remote_segments(app: &AxumRouter, id: Id, spans: &[(&str, i64, i64)]) {
        let body: Vec<serde_json::Value> = spans
            .iter()
            .map(|(text, start, end)| {
                serde_json::json!({
                    "text": text, "start_ms": start, "end_ms": end, "speaker": "Others",
                })
            })
            .collect();

        let (status, json) = call(
            app,
            post(
                &format!("/v1/meetings/{id}/transcript"),
                serde_json::Value::Array(body),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
    }

    async fn speakers_of(app: &AxumRouter, id: Id) -> Vec<Option<String>> {
        let (status, json) = call(app, get(&format!("/v1/meetings/{id}/transcript"))).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        json.as_array()
            .unwrap()
            .iter()
            .map(|s| s["speaker"].as_str().map(str::to_string))
            .collect()
    }

    /// The whole feature, end to end over HTTP: four remote people become four names.
    #[tokio::test]
    async fn posted_speaker_events_name_the_remote_channel_when_the_meeting_ends() {
        let app = app();
        let id = create_test_meeting(&app).await;

        add_remote_segments(
            &app,
            id,
            &[
                ("first", 0, 4_000),
                ("second", 5_000, 9_000),
                ("third", 10_000, 14_000),
                ("fourth", 15_000, 19_000),
            ],
        )
        .await;

        let (status, json) = call(
            &app,
            post(
                &format!("/v1/meetings/{id}/speaker-events"),
                serde_json::json!({
                    "participants": [
                        {"id": "p1", "display_name": "Priya"},
                        {"id": "p2", "display_name": "Marcus"},
                        {"id": "p3", "display_name": "Ana"},
                        {"id": "p4", "display_name": "Jun"},
                    ],
                    "turns": [
                        {"participant": "p1", "start_ms": 0,      "end_ms": 5_000},
                        {"participant": "p2", "start_ms": 5_000,  "end_ms": 10_000},
                        {"participant": "p3", "start_ms": 10_000, "end_ms": 15_000},
                        {"participant": "p4", "start_ms": 15_000, "end_ms": 20_000},
                    ],
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["participants_known"], 4);

        // Nothing is renamed until the meeting ends.
        assert_eq!(
            speakers_of(&app, id).await,
            vec![Some("Others".into()); 4],
            "posting events must not relabel mid-meeting"
        );

        let (status, _) = call(
            &app,
            post(&format!("/v1/meetings/{id}/end"), serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        assert_eq!(
            speakers_of(&app, id).await,
            vec![
                Some("Priya".into()),
                Some("Marcus".into()),
                Some("Ana".into()),
                Some("Jun".into()),
            ]
        );
    }

    /// The local user's turns must not name a segment from the tap that only carries other people.
    #[tokio::test]
    async fn the_local_participants_turns_do_not_name_remote_segments() {
        let app = app();
        let id = create_test_meeting(&app).await;

        add_remote_segments(&app, id, &[("theirs", 0, 4_000)]).await;

        let (status, json) = call(
            &app,
            post(
                &format!("/v1/meetings/{id}/speaker-events"),
                serde_json::json!({
                    "participants": [{"id": "me", "display_name": "Krishna"}],
                    "turns": [{"participant": "me", "start_ms": 0, "end_ms": 5_000}],
                    "local_participant_id": "me",
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");

        call(
            &app,
            post(&format!("/v1/meetings/{id}/end"), serde_json::json!({})),
        )
        .await;

        assert_eq!(
            speakers_of(&app, id).await,
            vec![Some("Others".into())],
            "the system tap does not carry the local user, so their turns must not label it"
        );
    }

    #[tokio::test]
    async fn a_turn_naming_an_unknown_participant_is_rejected() {
        let app = app();
        let id = create_test_meeting(&app).await;

        let (status, _) = call(
            &app,
            post(
                &format!("/v1/meetings/{id}/speaker-events"),
                serde_json::json!({
                    "participants": [{"id": "p1", "display_name": "Priya"}],
                    "turns": [{"participant": "ghost", "start_ms": 0, "end_ms": 1_000}],
                }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// A negative timestamp means the producer converted its clock wrongly.
    #[tokio::test]
    async fn a_turn_before_the_recording_started_is_rejected() {
        let app = app();
        let id = create_test_meeting(&app).await;

        let (status, _) = call(
            &app,
            post(
                &format!("/v1/meetings/{id}/speaker-events"),
                serde_json::json!({
                    "participants": [{"id": "p1", "display_name": "Priya"}],
                    "turns": [{"participant": "p1", "start_ms": -5_000, "end_ms": 1_000}],
                }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn speaker_events_for_an_unknown_meeting_are_a_404() {
        let (status, _) = call(
            &app(),
            post(
                "/v1/meetings/01890000000000000000000000/speaker-events",
                serde_json::json!({
                    "participants": [{"id": "p1", "display_name": "Priya"}],
                }),
            ),
        )
        .await;

        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST,
            "got {status}"
        );
    }

    #[tokio::test]
    async fn an_empty_batch_of_speaker_events_is_rejected() {
        let app = app();
        let id = create_test_meeting(&app).await;

        let (status, _) = call(
            &app,
            post(
                &format!("/v1/meetings/{id}/speaker-events"),
                serde_json::json!({}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// A meeting recorded without the extension must end exactly as it did before.
    #[tokio::test]
    async fn ending_a_meeting_with_no_speaker_events_changes_nothing() {
        let app = app();
        let id = create_test_meeting(&app).await;
        add_remote_segments(&app, id, &[("theirs", 0, 4_000)]).await;

        let (status, _) = call(
            &app,
            post(&format!("/v1/meetings/{id}/end"), serde_json::json!({})),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(speakers_of(&app, id).await, vec![Some("Others".into())]);
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

    /// Searching for a phrase someone remembers hearing has to lead back to the meeting. The
    /// hit is a transcript row, which no screen can open on its own — so it has to carry the
    /// meeting it belongs to, and borrow that meeting's name.
    #[tokio::test]
    async fn a_transcript_hit_names_the_meeting_it_can_be_opened_in() {
        let app = app();
        let (_, meeting) = call(
            &app,
            post(
                "/v1/meetings",
                serde_json::json!({"title": "Postgres migration sync"}),
            ),
        )
        .await;
        let meeting_id = meeting["id"].as_str().unwrap().to_string();

        call(
            &app,
            post(
                &format!("/v1/meetings/{meeting_id}/transcript"),
                serde_json::json!([{
                    "text": "Are we keeping the read replica after the split?",
                    "start_ms": 0,
                    "end_ms": 4000
                }]),
            ),
        )
        .await;

        let (status, hits) = call(&app, get("/v1/search?q=read%20replica")).await;
        assert_eq!(status, StatusCode::OK);

        let hits = hits.as_array().unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0]["kind"], "transcript_segment");
        assert_eq!(
            hits[0]["meeting_id"], meeting_id,
            "the hit must name the meeting it can be opened in"
        );
        assert_eq!(
            hits[0]["title"], "Postgres migration sync",
            "a segment has no title of its own, so it borrows the meeting's"
        );
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

    #[test]
    fn a_percent_encoded_filename_survives_the_header() {
        // Headers are ASCII; file names are not. This is why the client encodes at all.
        assert_eq!(
            percent_decode("R%C3%A9union%20d%27%C3%A9quipe.wav"),
            "Réunion d'équipe.wav"
        );
        assert_eq!(percent_decode("standup.wav"), "standup.wav");
        assert_eq!(percent_decode(""), "");
    }

    /// A malformed escape keeps its characters rather than failing. A file genuinely called
    /// "50%.wav" must import, not be rejected by its own name.
    #[test]
    fn a_stray_percent_is_kept_literally() {
        assert_eq!(percent_decode("50%.wav"), "50%.wav");
        assert_eq!(percent_decode("%zz.wav"), "%zz.wav");
        assert_eq!(percent_decode("%"), "%");
    }

    /// The name reaches a meeting title and a temp file's extension, never a path. Even so, the
    /// handler strips directories — a caller is not a file picker, and the cost of being wrong
    /// here is writing outside the import folder.
    #[test]
    fn a_traversing_filename_cannot_escape() {
        let decoded = percent_decode("..%2F..%2Fetc%2Fpasswd");
        assert_eq!(decoded, "../../etc/passwd");

        // What the handler does with it: take the last component only.
        let stripped = decoded.rsplit(['/', '\\']).next().unwrap();
        assert_eq!(stripped, "passwd");
        assert!(!stripped.contains('/'), "no separator may survive");
    }

    /// The key must never come back out through the API.
    ///
    /// It is accepted over loopback so a user can add a provider without editing a shell
    /// profile — but a readable key is a key in every screenshot, log and support bundle, so
    /// the listing reports only whether one exists.
    #[tokio::test]
    async fn a_saved_api_key_is_never_readable() {
        let app = app();

        let (status, body) = call(&app, get("/v1/backends")).await;
        assert_eq!(status, StatusCode::OK);

        for backend in body["backends"].as_array().unwrap() {
            assert!(
                backend["has_key"].is_boolean(),
                "presence must be reported as a bool, never as the value: {backend}"
            );
            // No field anywhere in an entry may carry a secret. Checked by shape rather than by
            // name, so a future field cannot smuggle one past a substring match.
            for (name, value) in backend.as_object().unwrap() {
                if name.contains("key") {
                    assert!(
                        value.is_boolean(),
                        "{name} must be a boolean, not something that could hold a secret: {value}"
                    );
                }
            }
        }

        // And there is no route that returns one.
        for path in ["/v1/backends/anthropic/key", "/v1/backends/groq/key"] {
            let (status, _) = call(&app, get(path)).await;
            assert!(
                status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::NOT_FOUND,
                "GET {path} must not exist, got {status}"
            );
        }
    }

    #[tokio::test]
    async fn a_key_is_refused_for_a_backend_that_does_not_use_one() {
        let (status, body) = call(
            &app(),
            post("/v1/backends/ollama/key", serde_json::json!({"key": "x"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    /// The window binds a new origin every launch, so anything kept in `localStorage` is gone
    /// by the next one. Preferences have to round-trip through the engine or a theme resets
    /// every time the app opens.
    #[tokio::test]
    async fn preferences_round_trip() {
        let app = app();

        let (status, empty) = call(&app, get("/v1/preferences")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            empty,
            serde_json::json!({}),
            "absent preferences are an empty object"
        );

        let wanted = serde_json::json!({"mode": "dark", "accent": "amber"});
        let (status, _) = call(&app, post("/v1/preferences", wanted.clone())).await;
        assert_eq!(status, StatusCode::OK);

        let (_, read_back) = call(&app, get("/v1/preferences")).await;
        assert_eq!(read_back, wanted);
    }

    /// A backend chosen in the app has to outlive the process.
    ///
    /// Without this the engine came back on its inferred default every launch, so a user whose
    /// Ollama holds `llama3.1:8b` — not the `llama3.1` the default guesses — fixed the same
    /// "model not found" error every single time they opened the app.
    #[tokio::test]
    async fn switching_backend_is_remembered() {
        let db = Database::open_in_memory().expect("db");
        let state = std::sync::Arc::new(AppState::new(
            db,
            AiRouter::from_config(RouterConfig::mock()).expect("router"),
        ));

        state
            .switch_backend(BackendKind::Ollama, Some("llama3.1:8b".into()), None)
            .await
            .expect("switch");

        let db = state.db().await;
        let settings = SettingsRepository::new(&db);
        assert_eq!(
            settings
                .get(crate::state::BACKEND_KIND_KEY)
                .unwrap()
                .as_deref(),
            Some("ollama"),
            "the chosen provider must be written down"
        );
        assert_eq!(
            settings
                .get(crate::state::BACKEND_MODEL_KEY)
                .unwrap()
                .as_deref(),
            Some("llama3.1:8b"),
            "the exact model tag must be written down, not just the provider"
        );
    }

    /// The mock backend answers every request with fixed text. Offering it in a menu means a
    /// user can end up reading a fabricated summary of a real meeting, formatted exactly like a
    /// real one, with nothing on screen saying so.
    #[tokio::test]
    async fn the_mock_backend_is_not_offered_and_cannot_be_selected() {
        let app = app();

        let (_, listed) = call(&app, get("/v1/backends")).await;
        let kinds: Vec<&str> = listed["backends"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["kind"].as_str().unwrap())
            .collect();
        assert!(
            !kinds.contains(&"mock"),
            "mock must not be listed: {kinds:?}"
        );
        assert!(
            kinds.contains(&"ollama"),
            "real backends must still be listed"
        );

        // Not merely hidden. A client that knows the name must still be refused.
        let (status, body) = call(
            &app,
            post("/v1/backend", serde_json::json!({"kind": "mock"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    /// A stopped Ollama is an ordinary state, not a failure. The picker has to be able to say
    /// "not running" — if this returned an error the whole settings screen would show a banner
    /// instead of a list.
    #[tokio::test]
    async fn listing_models_for_an_unreachable_daemon_is_not_an_error() {
        let (status, body) = call(&app(), get("/v1/backends/ollama/models")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["models"].as_array().unwrap().is_empty() || body["available"] == true);
    }

    #[tokio::test]
    async fn a_backend_that_cannot_list_models_says_so_rather_than_failing() {
        let (status, body) = call(&app(), get("/v1/backends/anthropic/models")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["available"], false);
        assert!(
            body["reason"].as_str().unwrap().contains("Anthropic"),
            "{body}"
        );
    }

    /// Whisper's names mean nothing to a user. Every entry has to explain what choosing it
    /// costs, or the model list is a quiz rather than a choice.
    #[tokio::test]
    async fn every_model_explains_what_it_is_for() {
        let (status, body) = call(&app(), get("/v1/models")).await;
        assert_eq!(status, StatusCode::OK);

        for model in body["models"].as_array().unwrap() {
            let name = model["name"].as_str().unwrap();
            assert!(
                model["tradeoff"].as_str().is_some_and(|t| t.len() > 40),
                "{name} has no usable description"
            );
            assert!(
                model["language_note"]
                    .as_str()
                    .is_some_and(|t| !t.is_empty()),
                "{name} does not say what its .en suffix means"
            );
        }
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

    // ------------------------------------------------------------------ setup

    /// An engine whose model directory is `dir`.
    fn app_with_model_dir(dir: &std::path::Path) -> AxumRouter {
        let state = AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        )
        .with_model_dir(dir);
        router(Arc::new(state))
    }

    /// Install a model fixture the store will actually accept.
    ///
    /// `ModelStore::is_available` compares the file's exact byte length against the
    /// catalogue, so a short placeholder never registers as installed. `set_len` produces the
    /// right length without writing 77 MB of zeros.
    fn install_model(dir: &std::path::Path, name: &str) -> notewise_transcription::ModelInfo {
        use notewise_transcription::ModelStore;

        let model = ModelRegistry::get(name).expect("a registry model");
        std::fs::File::create(ModelStore::new(dir).path_for(&model))
            .expect("create the fixture")
            .set_len(model.bytes)
            .expect("size the fixture");
        model
    }

    /// `/v1/models` and `/v1/models/:name/download` must agree about where models live. They
    /// did not: the listing re-derived a path from the environment while the downloader
    /// honoured `with_model_dir`, so a model on disk could be reported missing forever — and
    /// the setup gate that waits on it would never open.
    #[tokio::test]
    async fn list_models_honours_the_configured_model_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let model = install_model(dir.path(), "tiny.en");

        let (status, body) = json(
            &app_with_model_dir(dir.path()),
            Request::get("/v1/models").body(Body::empty()).unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["directory"].as_str().unwrap(),
            dir.path().display().to_string(),
            "the listing must report the configured directory"
        );

        let listed = body["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == model.name.as_str())
            .expect("tiny.en is in the registry");

        assert_eq!(
            listed["installed"], true,
            "a model in the configured directory must list as installed"
        );
    }

    #[tokio::test]
    async fn setup_reports_an_unfinished_first_run() {
        let dir = tempfile::tempdir().expect("tempdir");

        let (status, body) = json(
            &app_with_model_dir(dir.path()),
            Request::get("/v1/setup").body(Body::empty()).unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(
            body["completed_at"].is_null(),
            "nothing has completed setup"
        );
        assert_eq!(
            body["steps"]["model"]["satisfied"], false,
            "empty model dir"
        );
        assert_eq!(body["steps"]["model"]["required"], true);

        // System audio has no working backend on any current build, so it must be excluded
        // from the gate rather than left permanently blocking.
        assert_eq!(
            body["steps"]["permissions"]["system_audio"]["status"],
            "unavailable"
        );
        assert_eq!(
            body["steps"]["permissions"]["system_audio"]["required"],
            false
        );
        assert!(body["steps"]["permissions"]["system_audio"]["detail"].is_string());
    }

    /// A GET must never raise a TCC dialog. The only defence in code is that the handler calls
    /// the non-prompting probe, so pin the status that produces.
    #[tokio::test]
    async fn setup_does_not_prompt_for_permissions() {
        let dir = tempfile::tempdir().expect("tempdir");

        let (_, body) = json(
            &app_with_model_dir(dir.path()),
            Request::get("/v1/setup").body(Body::empty()).unwrap(),
        )
        .await;

        let status = body["steps"]["permissions"]["microphone"]["status"]
            .as_str()
            .unwrap();
        assert!(
            status == "not_requested" || status == "unavailable",
            "a GET must not have asked the OS anything, got {status}"
        );
    }

    /// The gate must hold at the API, not only in the UI. A client calling this directly with
    /// nothing installed must be refused, and told which step is missing.
    #[tokio::test]
    async fn completing_setup_with_unsatisfied_steps_is_a_conflict() {
        let dir = tempfile::tempdir().expect("tempdir");

        let (status, body) = json(
            &app_with_model_dir(dir.path()),
            Request::post("/v1/setup/complete")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            body["error"].as_str().unwrap().contains("model"),
            "the refusal must name the missing step, got {}",
            body["error"]
        );
    }

    /// The gate refuses an unintended completion; it must not be a lock-out. Someone who cannot
    /// satisfy a step — a grant that belongs to an administrator, a model they do not want to
    /// download today — has to be able to reach the app, and the answer has to say what they
    /// skipped rather than report a clean setup.
    #[tokio::test]
    async fn setup_can_be_skipped_explicitly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_with_model_dir(dir.path());

        let (status, body) = json(
            &app,
            Request::post("/v1/setup/complete?skip=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["completed_at"].is_string(), "{body}");
        assert!(
            body["skipped"]
                .as_array()
                .expect("the skipped steps")
                .iter()
                .any(|step| step == "model"),
            "the answer must name what was skipped, got {body}"
        );

        // Skipping records that the wizard is done, not that the machine is capable. The banner
        // that nags afterwards reads this, so it has to keep telling the truth.
        let (status, readiness) =
            json(&app, Request::get("/v1/setup").body(Body::empty()).unwrap()).await;

        assert_eq!(status, StatusCode::OK);
        assert!(readiness["completed_at"].is_string(), "{readiness}");
        assert_eq!(
            readiness["steps"]["model"]["satisfied"], false,
            "skipping must not fake readiness, got {readiness}"
        );
    }

    #[tokio::test]
    async fn completing_setup_records_a_timestamp_and_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        install_model(dir.path(), "tiny.en");

        // One app, so both calls share a database — a fresh router per call would start from
        // an empty one and never exercise idempotency.
        let app = app_with_model_dir(dir.path());

        let (status, first) = json(
            &app,
            Request::post("/v1/setup/complete")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{first}");

        let stamp = first["completed_at"]
            .as_str()
            .expect("a timestamp")
            .to_string();
        assert!(!stamp.is_empty());

        let (status, second) = json(
            &app,
            Request::post("/v1/setup/complete")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            second["completed_at"].as_str().unwrap(),
            stamp,
            "completing twice must not move the timestamp"
        );
    }

    #[tokio::test]
    async fn an_unknown_permission_kind_is_a_400() {
        let (status, _) = json(
            &app(),
            Request::post("/v1/permissions/webcam")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// On a build without capture, asking for system audio must answer "unavailable" rather
    /// than fail — the wizard needs a reason to show, not an error banner.
    #[tokio::test]
    async fn requesting_system_audio_reports_unavailable_rather_than_failing() {
        let (status, body) = json(
            &app(),
            Request::post("/v1/permissions/system_audio")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "unavailable");
        assert_eq!(body["required"], false);
    }

    // ------------------------------------------------------------ naming speakers by hand

    /// Store segments with the labels a diarizer would have left.
    async fn add_labelled(app: &AxumRouter, id: Id, spans: &[(&str, &str, i64, i64)]) {
        let body: Vec<serde_json::Value> = spans
            .iter()
            .map(|(speaker, text, start, end)| {
                serde_json::json!({
                    "text": text, "start_ms": start, "end_ms": end, "speaker": speaker,
                })
            })
            .collect();

        let (status, json) = call(
            app,
            post(
                &format!("/v1/meetings/{id}/transcript"),
                serde_json::Value::Array(body),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
    }

    #[tokio::test]
    async fn listing_speakers_reports_weight_and_anonymity() {
        let app = app();
        let id = create_test_meeting(&app).await;
        add_labelled(
            &app,
            id,
            &[
                ("Speaker 1", "opening", 0, 4_000),
                ("Speaker 2", "a word", 4_000, 5_000),
                ("Speaker 1", "closing", 5_000, 9_000),
            ],
        )
        .await;

        let (status, json) = call(&app, get(&format!("/v1/meetings/{id}/speakers"))).await;
        assert_eq!(status, StatusCode::OK, "{json}");

        let speakers = json["speakers"].as_array().unwrap();
        assert_eq!(speakers.len(), 2);
        assert_eq!(speakers[0]["label"], "Speaker 1");
        assert_eq!(speakers[0]["segments"], 2);
        assert_eq!(speakers[0]["speaking_ms"], 8_000);
        assert_eq!(
            speakers[0]["anonymous"], true,
            "a diarizer label must be flagged so the UI can ask who it is"
        );
        // The one-second speaker is the one a user needs the weight to judge.
        assert_eq!(speakers[1]["speaking_ms"], 1_000);
    }

    #[tokio::test]
    async fn a_named_speaker_is_not_flagged_anonymous() {
        let app = app();
        let id = create_test_meeting(&app).await;
        add_labelled(&app, id, &[("Speaker 1", "hello", 0, 2_000)]).await;

        let (_, json) = call(
            &app,
            post(
                &format!("/v1/meetings/{id}/speakers/rename"),
                serde_json::json!({"from": "Speaker 1", "to": "Dana"}),
            ),
        )
        .await;

        assert_eq!(json["speakers"][0]["label"], "Dana");
        assert_eq!(json["speakers"][0]["anonymous"], false);
        assert_eq!(json["merged"], false);
        assert_eq!(json["segments_changed"], 1);
    }

    /// The whole point of the feature, over HTTP: a split cluster becomes one person.
    #[tokio::test]
    async fn renaming_onto_an_existing_name_merges_and_reports_it() {
        let app = app();
        let id = create_test_meeting(&app).await;
        add_labelled(
            &app,
            id,
            &[
                ("Speaker 1", "a", 0, 2_000),
                ("Speaker 3", "b", 2_000, 4_000),
                ("Speaker 1", "c", 4_000, 6_000),
            ],
        )
        .await;

        let rename = |from: &str, to: &str| {
            post(
                &format!("/v1/meetings/{id}/speakers/rename"),
                serde_json::json!({"from": from, "to": to}),
            )
        };

        let (status, json) = call(&app, rename("Speaker 1", "Dana")).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["merged"], false, "nothing to merge with yet");

        // Speaker 3 was Dana all along — the clustering split one person in two.
        let (status, json) = call(&app, rename("Speaker 3", "Dana")).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["merged"], true);
        assert_eq!(json["segments_changed"], 1);

        let speakers = json["speakers"].as_array().unwrap();
        assert_eq!(speakers.len(), 1, "got {speakers:?}");
        assert_eq!(speakers[0]["label"], "Dana");
        assert_eq!(speakers[0]["segments"], 3);
    }

    /// A stale label means the caller's view is out of date, not that the meeting is wrong.
    #[tokio::test]
    async fn renaming_a_label_that_is_not_there_is_a_404() {
        let app = app();
        let id = create_test_meeting(&app).await;
        add_labelled(&app, id, &[("Speaker 1", "hello", 0, 2_000)]).await;

        let (status, _) = call(
            &app,
            post(
                &format!("/v1/meetings/{id}/speakers/rename"),
                serde_json::json!({"from": "Speaker 7", "to": "Dana"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_blank_speaker_name_is_rejected_at_the_boundary() {
        let app = app();
        let id = create_test_meeting(&app).await;
        add_labelled(&app, id, &[("Speaker 1", "hello", 0, 2_000)]).await;

        let (status, _) = call(
            &app,
            post(
                &format!("/v1/meetings/{id}/speakers/rename"),
                serde_json::json!({"from": "Speaker 1", "to": "  "}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn speakers_of_an_unknown_meeting_are_a_404() {
        let app = app();
        // A well-formed id that names nothing, so this tests absence rather than parsing.
        let unknown = "00000000-0000-0000-0000-000000000000";

        let (status, _) = call(&app, get(&format!("/v1/meetings/{unknown}/speakers"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// Renaming rewrites the search index, so a colleague's name finds what they said.
    #[tokio::test]
    async fn a_renamed_speaker_becomes_searchable_under_the_new_name() {
        let app = app();
        let id = create_test_meeting(&app).await;
        add_labelled(
            &app,
            id,
            &[("Speaker 1", "the quarterly numbers", 0, 2_000)],
        )
        .await;

        call(
            &app,
            post(
                &format!("/v1/meetings/{id}/speakers/rename"),
                serde_json::json!({"from": "Speaker 1", "to": "Dana"}),
            ),
        )
        .await;

        let (status, hits) = call(&app, get("/v1/search?q=Dana")).await;
        assert_eq!(status, StatusCode::OK, "{hits}");
        assert!(
            !hits.as_array().unwrap().is_empty(),
            "renaming should have reindexed: {hits}"
        );
    }

    // ------------------------------------------------------------ acoustic separation

    fn put(uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("PUT")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    }

    /// A guess must never be the default answer to "who spoke".
    #[tokio::test]
    async fn acoustic_separation_is_off_until_someone_turns_it_on() {
        let (status, body) = call(&app(), get("/v1/diarization")).await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["mode"], "off");
        assert_eq!(body["effective"], false);
        assert_eq!(body["blocked_by"], "Speaker separation is turned off.");
        assert_eq!(body["retain_minutes"], 90);
    }

    /// Turning it on is allowed even when it cannot run yet — but it must say why it will not.
    #[tokio::test]
    async fn turning_it_on_reports_what_is_still_missing() {
        let app = app();
        let (status, body) = call(
            &app,
            put("/v1/diarization", serde_json::json!({"mode": "acoustic"})),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["mode"], "acoustic");
        assert_eq!(body["effective"], false, "nothing is downloaded: {body}");

        // Whichever of the two conditions this build fails, it must name it rather than going
        // quiet — "on, and nothing happens" is the state this field exists to prevent.
        let reason = body["blocked_by"].as_str().expect("a reason");
        assert!(
            reason.contains("compiled without") || reason.contains("not been downloaded"),
            "unhelpful reason: {reason}"
        );
        assert_eq!(body["supported"], cfg!(feature = "speaker-diarization"));
    }

    #[tokio::test]
    async fn the_setting_survives_a_round_trip() {
        let app = app();
        call(
            &app,
            put(
                "/v1/diarization",
                serde_json::json!({"mode": "acoustic", "retain_minutes": 30}),
            ),
        )
        .await;

        let (_, body) = call(&app, get("/v1/diarization")).await;
        assert_eq!(body["mode"], "acoustic");
        assert_eq!(body["retain_minutes"], 30);
    }

    #[tokio::test]
    async fn an_unknown_speaker_model_is_rejected() {
        let (status, _) = call(
            &app(),
            put("/v1/diarization", serde_json::json!({"model": "nonesuch"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// A zero budget would mean the pass silently never runs; a huge one would try to hold the
    /// machine's whole memory.
    #[tokio::test]
    async fn an_out_of_range_retention_budget_is_rejected() {
        for bad in [0, -1, 100_000] {
            let (status, _) = call(
                &app(),
                put(
                    "/v1/diarization",
                    serde_json::json!({"retain_minutes": bad}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "for {bad}");
        }
    }

    /// A rejected field must not leave half the change applied.
    #[tokio::test]
    async fn a_rejected_update_changes_nothing() {
        let app = app();
        let (status, _) = call(
            &app,
            put(
                "/v1/diarization",
                serde_json::json!({"mode": "acoustic", "model": "nonesuch"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (_, body) = call(&app, get("/v1/diarization")).await;
        assert_eq!(body["mode"], "off", "the valid half must not have landed");
    }

    /// Against a temporary directory: asserting "nothing is installed" while reading the real
    /// model directory makes the result depend on whatever the developer has downloaded.
    #[tokio::test]
    async fn speaker_models_are_listed_with_install_state_and_a_tradeoff() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (status, body) = call(&app_with_model_dir(dir.path()), get("/v1/speaker-models")).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let models = body["models"].as_array().expect("models");
        assert!(models.len() >= 3, "{body}");

        for model in models {
            assert!(model["bytes"].as_u64().unwrap() > 1_000_000, "{model}");
            // A menu of three names and three sizes is a quiz, not a choice.
            assert!(!model["tradeoff"].as_str().unwrap().is_empty(), "{model}");
            assert_eq!(model["installed"], false, "nothing is downloaded in a test");
        }

        assert_eq!(
            models.iter().filter(|m| m["recommended"] == true).count(),
            1,
            "exactly one model should be recommended"
        );
        assert_eq!(
            models.iter().filter(|m| m["selected"] == true).count(),
            1,
            "the default should be selected when nothing has been chosen"
        );
    }

    #[tokio::test]
    async fn an_unknown_speaker_model_cannot_be_downloaded_or_removed() {
        let app = app();

        let (status, _) = call(
            &app,
            post(
                "/v1/speaker-models/nonesuch/download",
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = call(
            &app,
            Request::builder()
                .method("DELETE")
                .uri("/v1/speaker-models/nonesuch")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// Removing a model that was never downloaded is a no-op, not an error — the caller's intent
    /// is "make it not be there", and it already is not.
    ///
    /// Runs against a temporary model directory, and must stay that way. `app()` resolves the
    /// *real* one, so this test deleted the developer's actual downloaded model every time the
    /// suite ran — silently, since the assertion passes either way.
    #[tokio::test]
    async fn removing_a_model_that_is_not_installed_succeeds() {
        let dir = tempfile::tempdir().expect("temp dir");

        let (status, body) = call(
            &app_with_model_dir(dir.path()),
            Request::builder()
                .method("DELETE")
                .uri("/v1/speaker-models/campplus-voxceleb")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["removed"], "campplus-voxceleb");
    }

    /// An installed model is reported as installed, and removing it actually removes it.
    ///
    /// Isolated for the same reason as above: these are real files.
    #[tokio::test]
    async fn an_installed_speaker_model_can_be_removed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let model = notewise_diarization::SpeakerModelRegistry::default_model();

        // The store verifies by exact size, so the fixture has to be exactly that long.
        std::fs::write(
            dir.path().join(model.filename()),
            vec![0u8; model.bytes as usize],
        )
        .expect("fixture");

        let app = app_with_model_dir(dir.path());

        let (_, listed) = call(&app, get("/v1/speaker-models")).await;
        let entry = listed["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == model.name)
            .unwrap()
            .clone();
        assert_eq!(entry["installed"], true, "{entry}");

        call(
            &app,
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/speaker-models/{}", model.name))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert!(
            !dir.path().join(model.filename()).exists(),
            "the file should be gone"
        );
    }

    /// Speaker models and Whisper models share a directory, so their listings must not bleed.
    #[tokio::test]
    async fn the_two_model_catalogues_stay_separate() {
        let app = app();

        let (_, whisper) = call(&app, get("/v1/models")).await;
        let (_, speaker) = call(&app, get("/v1/speaker-models")).await;

        let names = |body: &serde_json::Value| -> Vec<String> {
            body["models"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["name"].as_str().unwrap().to_string())
                .collect()
        };

        let whisper_names = names(&whisper);
        let speaker_names = names(&speaker);

        assert!(whisper_names.contains(&"base.en".to_string()));
        assert!(speaker_names.contains(&"campplus-voxceleb".to_string()));
        for name in &speaker_names {
            assert!(
                !whisper_names.contains(name),
                "{name} appears in both catalogues"
            );
        }
    }
    /// The route out for a user with no mailbox connected, which is most people on a first run.
    #[tokio::test]
    async fn a_draft_downloads_as_a_message_a_mail_client_can_open() {
        let state = std::sync::Arc::new(AppState::new(
            Database::open_in_memory().expect("db"),
            AiRouter::from_config(RouterConfig::mock()).expect("router"),
        ));
        let app = router(std::sync::Arc::clone(&state));

        let draft_id = {
            let db = state.db().await;
            EmailDraftRepository::new(&db)
                .create(NewEmailDraft {
                    meeting_id: None,
                    subject: "Follow-up: Platform standup".into(),
                    body: "Here is what we agreed.".into(),
                    recipients: vec!["priya@example.com".into()],
                    variant: None,
                })
                .expect("a draft")
                .id
        };

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri(format!("/v1/emails/{draft_id}/eml"))
                    .body(axum::body::Body::empty())
                    .expect("builds"),
            )
            .await
            .expect("request");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("message/rfc822")
        );

        let disposition = response
            .headers()
            .get("content-disposition")
            .and_then(|v| v.to_str().ok())
            .expect("a filename");
        assert!(
            disposition.contains("Follow-up- Platform standup.eml"),
            "{disposition}"
        );

        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .expect("a body")
            .to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("To: priya@example.com"), "{text}");
        assert!(text.contains("X-Unsent: 1"), "{text}");
        assert!(text.contains("Here is what we agreed."), "{text}");
    }

    /// A subject is model-generated text, and the quote is the character that would end the header's
    /// own quoted string.
    #[test]
    fn a_filename_cannot_break_out_of_the_header() {
        let name = eml_file_name("Follow-up\" ; rm -rf /");
        assert!(!name.contains('"'), "{name}");
        assert!(!name.contains(';'), "{name}");
        assert!(name.ends_with(".eml"));
    }

    #[test]
    fn a_subject_with_nothing_usable_still_produces_a_filename() {
        assert_eq!(eml_file_name("///"), "draft.eml");
        assert_eq!(eml_file_name("   "), "draft.eml");
    }

    /// A two-hundred-character filename is refused by some filesystems outright.
    #[test]
    fn a_long_subject_is_shortened() {
        let name = eml_file_name(&"word ".repeat(60));
        assert!(name.len() <= 90, "{} chars: {name}", name.len());
    }
}
