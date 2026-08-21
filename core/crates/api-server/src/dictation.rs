//! Dictation: hold a key, talk, and the words land where the cursor is.
//!
//! Spec 9a, the one staged assistant feature the design recommends building. It reuses more than any
//! of the other three: the microphone source, the Whisper engine and its model registry, and the
//! microphone permission the app already asks for. The only genuinely new requirement is putting
//! text somewhere outside Notewise, which is [`notewise_os_input`]'s job.
//!
//! # Why this is in the engine and not the shell
//!
//! The design's architecture says nothing in the engine depends on `os-input`. This does, and the
//! reason is worth stating rather than hiding: with the `os-input` feature off — which is the
//! default, and what CI builds — that crate is pure logic with no platform SDK, no `unsafe`, and no
//! grant of any kind. What the design was protecting against is the assistant's *platform*
//! requirements leaking into an engine build, and a feature that is off does not leak.
//!
//! The alternative was putting the pipeline in the Tauri shell, which is outside the workspace and
//! cannot be tested at all. A tested feature behind a flag beats an untested one behind a wall.
//!
//! # Raw and cleaned
//!
//! Raw transcription needs no model beyond Whisper, so it is the one thing here that works with no
//! LLM configured at all. Cleaned sends the transcript to whichever backend is set to fix
//! punctuation and casing — which is a round trip and, on a hosted backend, an upload. So it is a
//! choice per session and not a default.
//!
//! # What a stop does, in order
//!
//! Stop capture, flush the engine, optionally clean, then insert. Insertion is last and its outcome
//! is reported: text that could not be inserted is still returned, because the words were said and
//! losing them is worse than not placing them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post, put};
use axum::{Json, Router as AxumRouter};
use chrono::{DateTime, Utc};
use notewise_macos_permissions::{Authorization, Capability};
use notewise_os_input::{
    aftermath, Binding, HotkeyRegistry, Insertion, OsInputError, ScreenContext,
};
use notewise_storage::SettingsRepository;
use notewise_transcription::Segment;
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

type Shared = Arc<AppState>;

/// Whether this build can capture and transcribe.
///
/// The same pair the recorder needs, and reported for the same reason: a client has no other way to
/// know, and a dictation button that silently does nothing is worse than one that is absent.
pub const SUPPORTED: bool = cfg!(all(feature = "record", feature = "whisper"));

/// Where the dictation hotkey is kept.
const HOTKEY_SETTING: &str = "assistant.dictation.hotkey";
/// Where the default output mode is kept.
const MODE_SETTING: &str = "assistant.dictation.mode";

/// The default binding.
///
/// Chosen from what nothing else claims. `super+shift+d` is not in the OS's set, not a common editor
/// shortcut, and not one of the combinations [`notewise_os_input::AVOID_BY_DEFAULT`] lists — which a
/// test checks, because a default hotkey that collides with somebody's IDE is an uninstall.
pub const DEFAULT_HOTKEY: &str = "super+shift+d";

/// The feature name the hotkey registry knows this by.
pub const FEATURE: &str = "dictation";

/// How long one dictation may run before it stops itself.
///
/// A session is a sentence or two. A hotkey pressed by accident and never pressed again would
/// otherwise hold the microphone open until the app quits, which is both a recording nobody asked
/// for and a battery complaint.
pub const MAX_SESSION_SECS: i64 = 120;

pub fn routes() -> AxumRouter<Shared> {
    AxumRouter::new()
        .route(
            "/v1/dictation",
            get(status).post(start).delete(stop_and_insert),
        )
        .route("/v1/dictation/cancel", post(cancel))
        .route("/v1/assistant", get(capabilities))
        .route("/v1/assistant/hotkey", put(set_hotkey))
        .route("/v1/assistant/context", get(read_context))
}

// ---------------------------------------------------------------- the pure parts

/// What to do with the words before they are inserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Exactly what was heard. Needs no model beyond Whisper, so it works with nothing configured.
    #[default]
    Raw,
    /// Punctuation and casing fixed by the configured backend.
    Cleaned,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Raw => "raw",
            Mode::Cleaned => "cleaned",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "raw" => Some(Mode::Raw),
            "cleaned" => Some(Mode::Cleaned),
            _ => None,
        }
    }
}

/// Turn what the engine produced into one piece of dictated text.
///
/// Whisper emits segments with their own leading spaces and occasional empty ones. Joining them
/// naively gives double spaces and a leading one, which in a text field is visible and looks like a
/// bug in the app rather than in the model.
pub fn join_segments(segments: &[Segment]) -> String {
    let mut out = String::new();

    for segment in segments {
        let text = segment.text.trim();
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(text);
    }

    out
}

/// The instruction for the cleaned mode.
///
/// Deliberately narrow. A model asked to "improve" dictation rewrites it, and a dictation surface
/// that changes the user's words is worse than one that leaves a missing comma — they said what
/// they said, and this is not a drafting tool.
pub fn cleanup_prompt() -> &'static str {
    "You fix the punctuation and capitalisation of dictated speech. Return the corrected text and \
nothing else — no preamble, no quotes, no commentary.

Rules:
- Do not change the words. Do not add, remove, reorder, or rephrase anything.
- Add sentence punctuation and capitals where speech implies them.
- Remove filler only where it is clearly a stumble: a repeated word, or a stray \"um\".
- If the text is already correct, return it unchanged."
}

/// Whether a cleaned result is safe to use, or whether the model rewrote instead of correcting.
///
/// Compared by word count rather than by characters: punctuation *should* change the length, and a
/// model that added three commas has done its job. One that came back half as long has rewritten,
/// and the raw transcript is the honest thing to insert.
///
/// A dictation surface that quietly changes what somebody said is worse than one that leaves a
/// missing comma, so the guard is deliberately strict and fails towards the raw text.
pub fn cleaning_is_trustworthy(raw: &str, cleaned: &str) -> bool {
    if cleaned.trim().is_empty() {
        return false;
    }

    let before = raw.split_whitespace().count();
    let after = cleaned.split_whitespace().count();

    if before == 0 {
        return false;
    }

    // A quarter either way covers removing stumbles and splitting a run-on; more than that is not
    // a correction.
    let ratio = after as f64 / before as f64;
    (0.75..=1.25).contains(&ratio)
}

// ---------------------------------------------------------------- the live session

/// A dictation in progress.
#[derive(Debug)]
struct Live {
    stop: Arc<AtomicBool>,
    started_at: DateTime<Utc>,
    mode: Mode,
    /// The transcript, when the capture thread is finished with it.
    done: oneshot::Receiver<std::result::Result<Vec<Segment>, String>>,
}

/// The at-most-one dictation.
///
/// One at a time on purpose: there is one microphone and one cursor, and a second session would be
/// competing for both.
#[derive(Debug, Default)]
pub struct DictationManager {
    live: Mutex<Option<Live>>,
}

impl DictationManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether something is being dictated, and since when.
    pub async fn status(&self) -> Option<(DateTime<Utc>, Mode)> {
        self.live
            .lock()
            .await
            .as_ref()
            .map(|live| (live.started_at, live.mode))
    }
}

#[derive(Debug, Serialize)]
struct StatusBody {
    /// Whether this build can dictate at all.
    supported: bool,
    /// Why not, when it cannot.
    reason: Option<String>,
    listening: bool,
    started_at: Option<String>,
    mode: Option<&'static str>,
}

async fn status(State(state): State<Shared>) -> ApiResult<Json<StatusBody>> {
    let live = state.dictation().status().await;

    Ok(Json(StatusBody {
        supported: SUPPORTED,
        reason: (!SUPPORTED).then(unsupported_reason),
        listening: live.is_some(),
        started_at: live.map(|(at, _)| at.to_rfc3339()),
        mode: live.map(|(_, mode)| mode.as_str()),
    }))
}

fn unsupported_reason() -> String {
    "This build cannot dictate: it was made without microphone capture and local transcription. \
     Install a build with the 'full' feature set."
        .to_string()
}

/// `device` and `language` are read only by the capture path, which is behind the feature flags —
/// so a build that cannot record accepts them and has nothing to do with them. Kept rather than
/// gated so the request shape does not change between builds, which would make one client work
/// against one engine and not another.
#[cfg_attr(
    not(all(feature = "record", feature = "whisper")),
    allow(dead_code, reason = "read only by the gated capture path")
)]
#[derive(Debug, Deserialize, Default)]
struct StartBody {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    device: Option<String>,
    #[serde(default)]
    language: Option<String>,
}

/// Start listening.
async fn start(
    State(state): State<Shared>,
    body: Option<Json<StartBody>>,
) -> ApiResult<Json<StatusBody>> {
    let body = body.map(|Json(body)| body).unwrap_or_default();

    if !SUPPORTED {
        return Err(ApiError::NotImplemented(unsupported_reason()));
    }

    let mode = match &body.mode {
        Some(raw) => Mode::parse(raw)
            .ok_or_else(|| ApiError::BadRequest(format!("'{raw}' is not a mode")))?,
        None => stored_mode(&state).await,
    };

    // Checked before the device is opened so the message is about the permission rather than about
    // a missing input, which is what a denied microphone looks like from `cpal`.
    if notewise_macos_permissions::microphone() == Authorization::Denied {
        return Err(ApiError::Conflict(Capability::Microphone.how_to_grant()));
    }

    let mut live = state.dictation().live.lock().await;
    if live.is_some() {
        return Err(ApiError::Conflict(
            "Notewise is already listening. Stop that dictation first.".into(),
        ));
    }

    let session =
        spawn_capture(&body, mode, state.model_dir().to_path_buf()).map_err(ApiError::Internal)?;
    let started_at = session.started_at;
    *live = Some(session);

    Ok(Json(StatusBody {
        supported: true,
        reason: None,
        listening: true,
        started_at: Some(started_at.to_rfc3339()),
        mode: Some(mode.as_str()),
    }))
}

#[derive(Debug, Serialize)]
struct DictatedBody {
    /// What was heard, after cleaning if that was asked for.
    text: String,
    /// What was heard before cleaning. Present only when the two differ.
    raw_text: Option<String>,
    mode: &'static str,
    /// How the text got where it went, or why it did not.
    insertion: Option<Insertion>,
    /// One sentence for the user, when there is something worth saying — a borrowed clipboard, or
    /// a refusal and what to do about it.
    note: Option<String>,
    duration_ms: i64,
}

/// Stop listening, then put the words at the cursor.
///
/// The text comes back either way. If insertion refused, the words were still said, and returning
/// nothing because they could not be placed would lose them.
async fn stop_and_insert(State(state): State<Shared>) -> ApiResult<Json<DictatedBody>> {
    let (segments, started_at, mode) = take_session(&state).await?;
    let duration_ms = (Utc::now() - started_at).num_milliseconds();

    let raw = join_segments(&segments);
    if raw.trim().is_empty() {
        return Ok(Json(DictatedBody {
            text: String::new(),
            raw_text: None,
            mode: mode.as_str(),
            insertion: None,
            note: Some("Nothing was heard.".into()),
            duration_ms,
        }));
    }

    let (text, raw_text) = match mode {
        Mode::Raw => (raw, None),
        Mode::Cleaned => match clean(&state, &raw).await {
            Some(cleaned) => (cleaned, Some(raw)),
            None => (raw, None),
        },
    };

    let (insertion, note) = match notewise_os_input::insert_at_cursor(&text) {
        Ok(outcome) => {
            let note = aftermath(&outcome);
            (Some(outcome), note)
        }
        // A missing grant or an uncompiled platform layer. The text is still returned, and the
        // message says which pane to open — the whole point of that error carrying it.
        Err(error) => (None, Some(insertion_note(&error))),
    };

    Ok(Json(DictatedBody {
        text,
        raw_text,
        mode: mode.as_str(),
        insertion,
        note,
        duration_ms,
    }))
}

/// What to tell the user when insertion could not even be attempted.
fn insertion_note(error: &OsInputError) -> String {
    format!("{error} Your text is above — copy it from here.")
}

/// Stop listening and throw the words away.
async fn cancel(State(state): State<Shared>) -> ApiResult<Json<serde_json::Value>> {
    let (_, started_at, _) = take_session(&state).await?;
    Ok(Json(serde_json::json!({
        "cancelled": true,
        "duration_ms": (Utc::now() - started_at).num_milliseconds(),
    })))
}

/// End the live session and wait for the transcript.
async fn take_session(state: &Shared) -> ApiResult<(Vec<Segment>, DateTime<Utc>, Mode)> {
    let session = state
        .dictation()
        .live
        .lock()
        .await
        .take()
        .ok_or_else(|| ApiError::Conflict("Notewise is not listening.".into()))?;

    session.stop.store(true, Ordering::Relaxed);

    let segments = session
        .done
        .await
        .map_err(|_| ApiError::Internal("the dictation thread stopped unexpectedly".into()))?
        .map_err(ApiError::Internal)?;

    Ok((segments, session.started_at, session.mode))
}

/// Ask the configured backend to fix the punctuation.
///
/// `None` when it could not, or when what came back is not a correction. Failing to a raw
/// transcript is right: the words are the user's and a model that rewrote them is worse than a
/// missing comma.
async fn clean(state: &Shared, raw: &str) -> Option<String> {
    use notewise_ai_router::{AiBackend, ChatMessage, ChatRequest};

    let request = ChatRequest::new(vec![ChatMessage::user(raw)])
        .with_context(vec![cleanup_prompt().to_string()]);

    let reply = match state.ai().chat(&request).await {
        Ok(reply) => reply.text.trim().to_string(),
        Err(error) => {
            tracing::info!(%error, "inserting the raw transcript; cleaning failed");
            return None;
        }
    };

    if cleaning_is_trustworthy(raw, &reply) {
        Some(reply)
    } else {
        tracing::info!("inserting the raw transcript; the model rewrote rather than corrected");
        None
    }
}

async fn stored_mode(state: &Shared) -> Mode {
    let db = state.db().await;
    SettingsRepository::new(&db)
        .get(MODE_SETTING)
        .ok()
        .flatten()
        .and_then(|raw| Mode::parse(&raw))
        .unwrap_or_default()
}

// ---------------------------------------------------------------- capabilities and settings

#[derive(Debug, Serialize)]
struct PermissionBody {
    capability: &'static str,
    label: &'static str,
    status: &'static str,
    /// What to do about it, when there is something to do.
    how_to_grant: Option<String>,
    settings_url: &'static str,
}

#[derive(Debug, Serialize)]
struct CapabilitiesBody {
    /// Whether this build can capture and transcribe.
    can_dictate: bool,
    /// Whether this build can put text into another application.
    can_insert: bool,
    reason: Option<String>,
    hotkey: String,
    mode: &'static str,
    permissions: Vec<PermissionBody>,
}

/// What the assistant can do on this machine, and what is stopping it.
///
/// One request so a settings screen does not have to assemble four answers and guess at the fifth.
async fn capabilities(State(state): State<Shared>) -> ApiResult<Json<CapabilitiesBody>> {
    let hotkey = stored_hotkey(&state).await;
    let mode = stored_mode(&state).await;

    // Only the two the assistant actually needs. Screen recording and input monitoring belong to
    // 9b and 9d, which are not built — listing them here would ask for grants nothing uses.
    let permissions = [Capability::Microphone, Capability::Accessibility]
        .into_iter()
        .map(|capability| {
            let status = notewise_macos_permissions::status(capability);
            PermissionBody {
                capability: match capability {
                    Capability::Microphone => "microphone",
                    Capability::Accessibility => "accessibility",
                    Capability::ScreenRecording => "screen_recording",
                    Capability::InputMonitoring => "input_monitoring",
                },
                label: capability.label(),
                status: match status {
                    Authorization::Granted => "granted",
                    Authorization::Denied => "denied",
                    Authorization::NotDetermined => "not_determined",
                    Authorization::Unknown => "unknown",
                },
                how_to_grant: (status != Authorization::Granted).then(|| capability.how_to_grant()),
                settings_url: capability.settings_url(),
            }
        })
        .collect();

    Ok(Json(CapabilitiesBody {
        can_dictate: SUPPORTED,
        can_insert: notewise_os_input::SUPPORTED,
        reason: (!SUPPORTED).then(unsupported_reason),
        hotkey,
        mode: mode.as_str(),
        permissions,
    }))
}

async fn stored_hotkey(state: &Shared) -> String {
    let db = state.db().await;
    SettingsRepository::new(&db)
        .get(HOTKEY_SETTING)
        .ok()
        .flatten()
        .unwrap_or_else(|| DEFAULT_HOTKEY.to_string())
}

#[derive(Debug, Deserialize)]
struct HotkeyBody {
    hotkey: String,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Serialize)]
struct HotkeySaved {
    hotkey: String,
    mode: &'static str,
    /// Set when the binding is one the OS or a common editor usually claims.
    ///
    /// A warning and not a refusal: it is the user's machine, and somebody who wants `⌘⇧3` for
    /// dictation is allowed to have it. Saying so is the difference between informing them and
    /// deciding for them.
    warning: Option<String>,
}

/// Set the dictation hotkey.
///
/// Validated through the same registry the runtime uses, so a combination that would collide with
/// another Notewise feature is refused here — at configuration time, with both names in the
/// message — rather than at press time, when the symptom is a key that does nothing.
async fn set_hotkey(
    State(state): State<Shared>,
    Json(body): Json<HotkeyBody>,
) -> ApiResult<Json<HotkeySaved>> {
    let binding =
        Binding::parse(&body.hotkey).map_err(|error| ApiError::BadRequest(error.to_string()))?;

    // Every feature that holds a binding today. Dictation is the only one, and the registry is
    // consulted anyway so that adding the second one cannot forget to.
    let mut registry = HotkeyRegistry::new();
    registry
        .rebind(FEATURE, binding.clone())
        .map_err(|error| ApiError::Conflict(OsInputError::from(error).to_string()))?;

    let mode = match &body.mode {
        Some(raw) => Some(
            Mode::parse(raw)
                .ok_or_else(|| ApiError::BadRequest(format!("'{raw}' is not a mode")))?,
        ),
        None => None,
    };

    {
        let db = state.db().await;
        let settings = SettingsRepository::new(&db);
        settings
            .set(HOTKEY_SETTING, &binding.to_string())
            .map_err(|e| ApiError::Internal(format!("could not save the hotkey: {e}")))?;
        if let Some(mode) = mode {
            settings
                .set(MODE_SETTING, mode.as_str())
                .map_err(|e| ApiError::Internal(format!("could not save the mode: {e}")))?;
        }
    }

    let warning = notewise_os_input::is_commonly_claimed(&binding).then(|| {
        format!(
            "'{binding}' is usually claimed by the system or by editors. Notewise will try, and \
             whichever app registered it first wins."
        )
    });

    Ok(Json(HotkeySaved {
        hotkey: binding.to_string(),
        mode: mode.unwrap_or(Mode::Raw).as_str(),
        warning,
    }))
}

#[derive(Debug, Serialize)]
struct ContextBody {
    context: ScreenContext,
    /// The same thing reduced to one block, which is what a model would be given.
    prompt: String,
    /// Empty is a valid answer, so it is stated rather than inferred from an empty object.
    empty: bool,
}

/// What is on screen, as text.
///
/// Exists so the foundation's screen-context reader is reachable and inspectable before 9b is built
/// on top of it — and so a user can see exactly what an overlay assistant *would* be shown, which
/// is the kind of thing that should be visible before it is used rather than after.
async fn read_context() -> ApiResult<Json<ContextBody>> {
    let context = notewise_os_input::screen_context().map_err(|error| match error {
        OsInputError::PermissionRequired { .. } => ApiError::Conflict(error.to_string()),
        _ => ApiError::NotImplemented(error.to_string()),
    })?;

    Ok(Json(ContextBody {
        prompt: context.to_prompt(notewise_os_input::PROMPT_LIMIT),
        empty: context.is_empty(),
        context,
    }))
}

// ---------------------------------------------------------------- capture

/// Open the microphone and start transcribing into a buffer.
#[cfg(all(feature = "record", feature = "whisper"))]
fn spawn_capture(
    body: &StartBody,
    mode: Mode,
    model_dir: std::path::PathBuf,
) -> std::result::Result<Live, String> {
    use notewise_audio_capture::{AudioSource, CaptureConfig, MicrophoneSource};
    use notewise_transcription::{ModelRegistry, ModelStore, TranscriptionEngine, WhisperEngine};

    // The smallest installed model, or the catalogue's default when none is installed — the
    // availability check below is what turns that into a message the user can act on.
    let model = ModelRegistry::all()
        .into_iter()
        .find(|candidate| ModelStore::new(model_dir.clone()).is_available(candidate))
        .or_else(|| ModelRegistry::all().into_iter().next())
        .ok_or_else(|| "no transcription model is known to this build".to_string())?;
    // From state, not from the default: the desktop app keeps its models inside its own app-data
    // container, and looking in the wrong place reports a missing model that is actually installed.
    let store = ModelStore::new(model_dir);

    if !store.is_available(&model) {
        return Err(format!(
            "the '{}' model is not installed — download it first",
            model.name
        ));
    }

    let capture = CaptureConfig {
        device: body.device.clone(),
        ..Default::default()
    };
    let mut source = MicrophoneSource::open(&capture).map_err(|e| e.to_string())?;

    let mut engine = WhisperEngine::new(model, store)
        .map_err(|e| e.to_string())?
        .with_language(body.language.clone());

    let stop = Arc::new(AtomicBool::new(false));
    let poll = Arc::clone(&stop);
    let (tx, done) = oneshot::channel();
    let started_at = Utc::now();

    // A dedicated OS thread rather than a tokio task, for the same reason recording uses one:
    // `next_frame` blocks and Whisper inference is a long CPU burn, and either on a runtime worker
    // would stall unrelated requests.
    std::thread::Builder::new()
        .name("notewise-dictation".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().build() {
                Ok(runtime) => runtime,
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    return;
                }
            };

            let result = runtime.block_on(async move {
                let mut segments = Vec::new();
                let deadline = started_at + chrono::Duration::seconds(MAX_SESSION_SECS);

                loop {
                    if poll.load(Ordering::Relaxed) {
                        break;
                    }
                    // A hotkey pressed by accident must not hold the microphone until the app
                    // quits.
                    if Utc::now() >= deadline {
                        tracing::info!("dictation stopped itself at the session limit");
                        break;
                    }

                    match source.next_frame() {
                        Ok(Some(frame)) => {
                            let ready = engine.feed(&frame.to_transcription_format()).await;
                            match ready {
                                Ok(ready) => segments.extend(ready),
                                Err(e) => return Err(e.to_string()),
                            }
                        }
                        // A microphone never exhausts, so this means the device went away.
                        Ok(None) => break,
                        Err(e) => return Err(e.to_string()),
                    }
                }

                match engine.finish().await {
                    Ok(remaining) => segments.extend(remaining),
                    Err(e) => return Err(e.to_string()),
                }

                Ok(segments)
            });

            let _ = tx.send(result);
        })
        .map_err(|e| format!("could not start the dictation thread: {e}"))?;

    Ok(Live {
        stop,
        started_at,
        mode,
        done,
    })
}

/// The same shape for a build that cannot capture, so the caller compiles either way.
#[cfg(not(all(feature = "record", feature = "whisper")))]
fn spawn_capture(
    _body: &StartBody,
    _mode: Mode,
    _model_dir: std::path::PathBuf,
) -> std::result::Result<Live, String> {
    Err(unsupported_reason())
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

    fn segment(text: &str) -> Segment {
        Segment {
            text: text.to_string(),
            start_ms: 0,
            end_ms: 1_000,
            speaker: None,
            confidence: None,
        }
    }

    // ------------------------------------------------------------ joining

    /// Whisper's leading spaces and empty segments become a visible bug in a text field.
    #[test]
    fn segments_join_into_one_clean_line() {
        let segments = [
            segment(" Hello there"),
            segment(""),
            segment(" how are you"),
        ];
        assert_eq!(join_segments(&segments), "Hello there how are you");
    }

    #[test]
    fn nothing_heard_is_an_empty_string_rather_than_a_space() {
        assert_eq!(join_segments(&[]), "");
        assert_eq!(join_segments(&[segment("  "), segment("")]), "");
    }

    #[test]
    fn a_single_segment_is_trimmed() {
        assert_eq!(join_segments(&[segment("  spoken  ")]), "spoken");
    }

    // ------------------------------------------------------------ the cleaning guard

    /// The guard exists because a dictation surface that changes what somebody said is worse than
    /// one that leaves a missing comma.
    #[test]
    fn punctuation_is_a_correction_and_passes() {
        assert!(cleaning_is_trustworthy(
            "hello there how are you today",
            "Hello there. How are you today?"
        ));
    }

    #[test]
    fn removing_a_stumble_is_still_a_correction() {
        assert!(cleaning_is_trustworthy(
            "so um so I think we should ship it",
            "So I think we should ship it."
        ));
    }

    #[test]
    fn a_rewrite_is_refused_and_the_raw_words_win() {
        assert!(!cleaning_is_trustworthy(
            "hello there how are you today I hope things are going well for you",
            "Hi!"
        ));
        assert!(!cleaning_is_trustworthy(
            "ship it",
            "I would like to formally propose that we proceed with shipping this release today."
        ));
    }

    #[test]
    fn an_empty_answer_is_refused() {
        assert!(!cleaning_is_trustworthy("hello there", ""));
        assert!(!cleaning_is_trustworthy("hello there", "   "));
    }

    #[test]
    fn nothing_cannot_be_cleaned_into_something() {
        assert!(!cleaning_is_trustworthy("", "Hello."));
    }

    /// The prompt has to forbid rewriting explicitly, because that is what a model does otherwise.
    #[test]
    fn the_prompt_forbids_changing_the_words() {
        let prompt = cleanup_prompt();
        assert!(prompt.contains("Do not change the words"), "{prompt}");
        assert!(prompt.contains("nothing else"), "{prompt}");
    }

    // ------------------------------------------------------------ modes

    #[test]
    fn raw_is_the_default_because_it_needs_no_model() {
        assert_eq!(Mode::default(), Mode::Raw);
    }

    #[test]
    fn modes_round_trip_through_their_stored_spelling() {
        for mode in [Mode::Raw, Mode::Cleaned] {
            assert_eq!(Mode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(Mode::parse("improved"), None);
    }

    // ------------------------------------------------------------ the default hotkey

    /// A default hotkey that collides with somebody's IDE is an uninstall.
    #[test]
    fn the_default_hotkey_is_not_one_the_system_claims() {
        let binding = Binding::parse(DEFAULT_HOTKEY).expect("parses");
        assert!(
            !notewise_os_input::is_commonly_claimed(&binding),
            "{DEFAULT_HOTKEY} is in the list of combinations to avoid"
        );
        assert!(
            binding.modifiers().len() >= 2,
            "one modifier is too easy to hit"
        );
    }

    // ------------------------------------------------------------ over HTTP

    fn app() -> AxumRouter<()> {
        let state = AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        );
        routes().with_state(Arc::new(state))
    }

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

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("builds")
    }

    fn put(uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("PUT")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("builds")
    }

    /// A build that cannot dictate says so, rather than offering a button that does nothing.
    #[test]
    fn the_status_reports_what_this_build_can_do() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let app = app();
            let (status, body) = call(&app, get("/v1/dictation")).await;

            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["supported"], SUPPORTED);
            assert_eq!(body["listening"], false);

            if !SUPPORTED {
                assert!(
                    body["reason"].as_str().expect("a reason").contains("build"),
                    "{body}"
                );
            }
        });
    }

    #[test]
    fn capabilities_list_only_the_permissions_this_feature_uses() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let app = app();
            let (status, body) = call(&app, get("/v1/assistant")).await;
            assert_eq!(status, StatusCode::OK);

            let names: Vec<&str> = body["permissions"]
                .as_array()
                .expect("a list")
                .iter()
                .map(|p| p["capability"].as_str().expect("a name"))
                .collect();

            assert_eq!(names, vec!["microphone", "accessibility"]);
            assert!(
                !names.contains(&"input_monitoring"),
                "nothing built uses it, so asking for it would be asking for nothing"
            );
            assert_eq!(body["hotkey"], DEFAULT_HOTKEY);
            assert_eq!(body["can_insert"], notewise_os_input::SUPPORTED);
        });
    }

    /// A permission that is not granted comes with what to do about it, or the screen is a dead end.
    #[test]
    fn an_ungranted_permission_carries_its_instructions() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let app = app();
            let (_, body) = call(&app, get("/v1/assistant")).await;

            for permission in body["permissions"].as_array().expect("a list") {
                if permission["status"] != "granted" {
                    let how = permission["how_to_grant"].as_str().expect("instructions");
                    assert!(how.contains("System Settings"), "{how}");
                }
                assert!(permission["settings_url"]
                    .as_str()
                    .expect("a url")
                    .starts_with("x-apple"));
            }
        });
    }

    #[test]
    fn a_hotkey_can_be_set_and_comes_back() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let app = app();

            let (status, saved) = call(
                &app,
                put(
                    "/v1/assistant/hotkey",
                    serde_json::json!({ "hotkey": "CTRL+ALT+D", "mode": "cleaned" }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{saved}");
            // Normalised on the way in, so two spellings of one binding cannot both be stored.
            assert_eq!(saved["hotkey"], "ctrl+alt+d");
            assert_eq!(saved["mode"], "cleaned");
            assert_eq!(saved["warning"], serde_json::Value::Null);

            let (_, capabilities) = call(&app, get("/v1/assistant")).await;
            assert_eq!(capabilities["hotkey"], "ctrl+alt+d");
            assert_eq!(capabilities["mode"], "cleaned");
        });
    }

    /// It is the user's machine. A combination the OS usually claims is a warning, not a refusal.
    #[test]
    fn a_commonly_claimed_binding_is_allowed_with_a_warning() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let app = app();
            let (status, saved) = call(
                &app,
                put(
                    "/v1/assistant/hotkey",
                    serde_json::json!({ "hotkey": "super+space" }),
                ),
            )
            .await;

            assert_eq!(status, StatusCode::OK, "{saved}");
            let warning = saved["warning"].as_str().expect("a warning");
            assert!(warning.contains("first wins"), "{warning}");
        });
    }

    #[test]
    fn a_binding_with_no_modifier_is_refused_before_it_is_stored() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let app = app();
            let (status, _) = call(
                &app,
                put("/v1/assistant/hotkey", serde_json::json!({ "hotkey": "d" })),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);

            let (_, capabilities) = call(&app, get("/v1/assistant")).await;
            assert_eq!(capabilities["hotkey"], DEFAULT_HOTKEY, "nothing was stored");
        });
    }

    #[test]
    fn stopping_when_nothing_is_listening_is_a_conflict_and_not_a_crash() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let app = app();
            let request = Request::builder()
                .method("DELETE")
                .uri("/v1/dictation")
                .body(Body::empty())
                .expect("builds");

            let (status, _) = call(&app, request).await;
            assert_eq!(status, StatusCode::CONFLICT);
        });
    }

    /// Without the platform layer, reading the screen refuses with a reason rather than returning
    /// an empty context — which would read as "there was nothing on screen".
    #[test]
    fn reading_the_screen_says_why_it_cannot_rather_than_answering_nothing() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let app = app();
            let (status, body) = call(&app, get("/v1/assistant/context")).await;

            if notewise_os_input::SUPPORTED {
                // With the layer compiled in the answer depends on a grant this test cannot hold.
                assert!(
                    status == StatusCode::OK || status == StatusCode::CONFLICT,
                    "{status} {body}"
                );
            } else {
                assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
                assert!(body["error"]
                    .as_str()
                    .expect("a reason")
                    .contains("os-input"));
            }
        });
    }
}
