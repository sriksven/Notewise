//! Reading, writing and explaining the model routing policy.
//!
//! # Why this is a separate module
//!
//! `routes.rs` is already several thousand lines. Routing has its own validation rules, its own
//! failure modes, and one endpoint whose whole purpose is to be readable by a human — it earns a
//! file.
//!
//! # Why the explain endpoint exists
//!
//! Routing spends money on the user's behalf without being asked each time. "Why did that summary
//! cost anything" has to be answerable, or the honest response to a surprising bill is to turn
//! routing off. [`explain`] answers it for a hypothetical request, before any call is made.

use std::sync::Arc;

use axum::extract::State;
use axum::{
    routing::{get, post},
    Json, Router as AxumRouter,
};
use notewise_ai_router::{
    contradictory_route, unreachable_route, BackendKind, Predicate, RequestFacts, RouteSpec,
    StoredRoute, TaskKind,
};
use notewise_storage::{MergeMode, SettingsRepository};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, ROUTING_RULES_KEY};

type Shared = Arc<AppState>;

pub fn routes() -> AxumRouter<Shared> {
    AxumRouter::new()
        .route("/v1/routing/rules", get(get_rules).put(put_rules))
        .route("/v1/routing/explain", post(explain))
        .route("/v1/routing/default", post(install_default))
        .route("/v1/workspace/merge", post(merge_workspace))
        .route("/v1/notifications/pending", get(pending_notifications))
        .route("/v1/notifications/:id/delivered", post(mark_delivered))
        .route(
            "/v1/summary-templates",
            get(list_templates).post(create_template),
        )
        .route(
            "/v1/summary-templates/:id",
            axum::routing::put(update_template).delete(delete_template),
        )
        .route("/v1/meetings/:id/title", axum::routing::put(set_title))
        .route(
            "/v1/segments/:id/text",
            axum::routing::put(set_segment_text),
        )
        .route("/v1/audio/retention", get(get_retention).put(put_retention))
        .route("/v1/audio/sweep", post(sweep_audio))
        .route("/v1/meetings/:id/audio", get(serve_audio))
        .route("/v1/meetings/:id/audio/info", get(audio_info))
        .route("/v1/memories", get(list_memories).post(create_memory))
        .route(
            "/v1/memories/:id",
            axum::routing::put(update_memory).delete(delete_memory),
        )
}

#[derive(Debug, Serialize)]
struct MemoryBody {
    id: String,
    scope: String,
    project_id: Option<String>,
    text: String,
    /// `manual` or `extracted`. Shown so a user can tell what they wrote from what was inferred.
    origin: String,
    /// The meeting it came from, while that meeting exists. Answers "why does it think that".
    source_meeting_id: Option<String>,
    created_at: String,
}

impl From<notewise_storage::Memory> for MemoryBody {
    fn from(m: notewise_storage::Memory) -> Self {
        Self {
            id: m.id.to_string(),
            scope: m.scope.as_str().to_string(),
            project_id: m.project_id.map(|i| i.to_string()),
            text: m.text,
            origin: m.origin.as_str().to_string(),
            source_meeting_id: m.source_meeting_id.map(|i| i.to_string()),
            created_at: m.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize)]
struct MemoriesResponse {
    memories: Vec<MemoryBody>,
    /// How many of each scope are used, and the ceiling. A cap the user cannot see is a cap that
    /// arrives as a surprise refusal.
    global_used: usize,
    global_cap: usize,
    project_cap: usize,
}

async fn list_memories(State(state): State<Shared>) -> ApiResult<Json<MemoriesResponse>> {
    let db = state.db().await;
    let repo = notewise_storage::MemoryRepository::new(&db);
    let memories = repo.list()?;
    let global_used = repo.count(notewise_storage::MemoryScope::Global, None)?;

    Ok(Json(MemoriesResponse {
        memories: memories.into_iter().map(Into::into).collect(),
        global_used,
        global_cap: notewise_storage::GLOBAL_CAP,
        project_cap: notewise_storage::PROJECT_CAP,
    }))
}

#[derive(Debug, Deserialize)]
struct MemoryInput {
    text: String,
    /// `global` or `project`. Defaults to global, which is what a user typing a preference means.
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
}

/// Add a memory by hand.
///
/// Works whether or not automatic extraction is on. Typing something you want remembered is a
/// deliberate act and should not require enabling a background pass.
async fn create_memory(
    State(state): State<Shared>,
    Json(body): Json<MemoryInput>,
) -> ApiResult<Json<MemoryBody>> {
    let scope = match body.scope.as_deref().unwrap_or("global") {
        "global" => notewise_storage::MemoryScope::Global,
        "project" => notewise_storage::MemoryScope::Project,
        other => {
            return Err(ApiError::BadRequest(format!(
                "'{other}' is not a scope; expected global or project"
            )))
        }
    };

    let project_id = match body.project_id.as_deref() {
        Some(raw) => Some(parse_storage_id(raw)?),
        None => None,
    };

    let db = state.db().await;
    let made =
        notewise_storage::MemoryRepository::new(&db).create(notewise_storage::NewMemory {
            scope,
            project_id,
            text: body.text,
            origin: notewise_storage::MemoryOrigin::Manual,
            source_meeting_id: None,
        })?;

    Ok(Json(made.into()))
}

async fn update_memory(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<MemoryInput>,
) -> ApiResult<Json<MemoryBody>> {
    let id = parse_storage_id(&id)?;
    let db = state.db().await;
    let updated = notewise_storage::MemoryRepository::new(&db).update(id, &body.text)?;
    Ok(Json(updated.into()))
}

async fn delete_memory(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let id = parse_storage_id(&id)?;
    let db = state.db().await;
    notewise_storage::MemoryRepository::new(&db).delete(id)?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[derive(Debug, Serialize)]
struct AudioInfo {
    available: bool,
    bytes: i64,
}

/// Whether this meeting has audio to play.
///
/// A separate call rather than a field on the meeting: `Meeting` serialises straight from the model
/// and its queries read columns positionally, so adding one there means touching five `SELECT`s for
/// a fact only the transcript view asks about. Reports `available: false` rather than 404 so the
/// caller can distinguish "no audio" from "no such meeting".
async fn audio_info(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<AudioInfo>> {
    let meeting_id = parse_storage_id(&id)?;
    let db = state.db().await;

    match notewise_storage::audio_for(&db, meeting_id)? {
        // On disk, not merely pointed at: a player offered for a file that has gone shows a broken
        // control, which is worse than showing none.
        Some((path, bytes)) if std::path::Path::new(&path).is_file() => Ok(Json(AudioInfo {
            available: true,
            bytes,
        })),
        _ => Ok(Json(AudioInfo {
            available: false,
            bytes: 0,
        })),
    }
}

/// Serve a meeting's retained audio, honouring `Range`.
///
/// # Why ranges matter here
///
/// Clicking a transcript line seeks to a moment, and a browser cannot seek in a resource it has to
/// download whole first. An hour of retained audio is over two hundred megabytes; without ranges,
/// every seek would read all of it into memory on both sides. With them, the player fetches the few
/// hundred kilobytes around the moment asked for.
///
/// Only the bytes asked for are read from disk — the file is never loaded whole, whatever the
/// request.
async fn serve_audio(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<axum::response::Response, ApiError> {
    use axum::http::{header, StatusCode};
    use std::io::{Read, Seek, SeekFrom};

    let meeting_id = parse_storage_id(&id)?;
    let (path, _) = {
        let db = state.db().await;
        notewise_storage::audio_for(&db, meeting_id)?
    }
    .ok_or_else(|| ApiError::NotFound("no audio was kept for this meeting".into()))?;

    let mut file = std::fs::File::open(&path).map_err(|_| {
        // The pointer outlived the file. That reads as "no audio", and the next sweep clears it.
        ApiError::NotFound("the audio for this meeting is no longer on disk".into())
    })?;
    let total = file
        .metadata()
        .map_err(|e| ApiError::Internal(format!("could not read the audio file: {e}")))?
        .len();

    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| parse_byte_range(raw, total));

    let (start, end) = match range {
        Some(r) => r,
        None => (0, total.saturating_sub(1)),
    };
    if total == 0 || start > end || start >= total {
        return Err(ApiError::BadRequest(
            "that range is outside the audio file".into(),
        ));
    }

    let length = end - start + 1;
    file.seek(SeekFrom::Start(start))
        .map_err(|e| ApiError::Internal(format!("could not seek the audio file: {e}")))?;
    let mut body = vec![0u8; length as usize];
    file.read_exact(&mut body)
        .map_err(|e| ApiError::Internal(format!("could not read the audio file: {e}")))?;

    let partial = range.is_some();
    let mut response = axum::response::Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, "audio/wav")
        .header(header::CONTENT_LENGTH, length)
        // Advertised unconditionally: a player that does not see this will refuse to seek at all,
        // and then the whole feature silently does not work.
        .header(header::ACCEPT_RANGES, "bytes");

    if partial {
        response = response.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        );
    }

    response
        .body(axum::body::Body::from(body))
        .map_err(|e| ApiError::Internal(format!("could not build the audio response: {e}")))
}

/// Parse a single-range `bytes=` header against a known file length.
///
/// Pure, and the only part of range handling that can be subtly wrong, so it is tested directly.
/// Multi-range requests are not supported: browsers do not send them for media, and answering one
/// badly is worse than declining to.
fn parse_byte_range(raw: &str, total: u64) -> Option<(u64, u64)> {
    let spec = raw.strip_prefix("bytes=")?.trim();
    if spec.contains(',') || total == 0 {
        return None;
    }

    let (from, to) = spec.split_once('-')?;
    let last = total - 1;

    match (from.trim(), to.trim()) {
        // `bytes=-500` — the final 500 bytes.
        ("", suffix) => {
            let len: u64 = suffix.parse().ok()?;
            if len == 0 {
                return None;
            }
            Some((total.saturating_sub(len), last))
        }
        // `bytes=500-` — from 500 to the end.
        (start, "") => {
            let start: u64 = start.parse().ok()?;
            (start <= last).then_some((start, last))
        }
        (start, end) => {
            let start: u64 = start.parse().ok()?;
            let end: u64 = end.parse().ok()?;
            // Clamped rather than rejected: a player asking past the end is asking for the tail,
            // and every browser does it on the last chunk.
            (start <= end && start <= last).then_some((start, end.min(last)))
        }
    }
}

#[derive(Debug, Serialize)]
struct RetentionBody {
    /// `off`, `until_deleted`, or `days:N`.
    policy: String,
    /// How many meetings currently have audio, and how much space it is using.
    retained: usize,
    bytes: i64,
    /// Whether retention can be enabled at all. False on an encrypted workspace.
    can_enable: bool,
    /// Why not, when it cannot.
    blocked_by: Option<String>,
}

async fn get_retention(State(state): State<Shared>) -> ApiResult<Json<RetentionBody>> {
    let db = state.db().await;
    let policy = notewise_storage::retention_policy(&db);

    let (retained, bytes) = notewise_storage::retained_totals(&db)?;

    let encrypted = db.is_encrypted();
    Ok(Json(RetentionBody {
        policy: policy.as_str(),
        retained,
        bytes,
        can_enable: !encrypted,
        blocked_by: encrypted.then(|| {
            "this workspace is encrypted, and retained audio would be written unencrypted \
             beside it"
                .to_string()
        }),
    }))
}

#[derive(Debug, Deserialize)]
struct RetentionInput {
    policy: String,
}

/// Change the retention policy.
///
/// Switching to `off` sweeps immediately rather than waiting for a later pass. A user who turns this
/// off has said they do not want the recordings, and leaving them until some timer fires would mean
/// the setting said one thing while the disk said another.
async fn put_retention(
    State(state): State<Shared>,
    Json(body): Json<RetentionInput>,
) -> ApiResult<Json<RetentionBody>> {
    let policy = notewise_storage::RetentionPolicy::parse(&body.policy);
    if policy == notewise_storage::RetentionPolicy::Off && body.policy.trim() != "off" {
        return Err(ApiError::BadRequest(format!(
            "'{}' is not a retention policy; expected off, until_deleted, or days:N",
            body.policy
        )));
    }

    {
        let db = state.db().await;
        notewise_storage::set_retention_policy(&db, policy)?;
        if policy == notewise_storage::RetentionPolicy::Off {
            notewise_storage::sweep(&db, policy, chrono::Utc::now())?;
        }
    }

    get_retention(State(state)).await
}

#[derive(Debug, Serialize)]
struct SweepBody {
    deleted: usize,
    bytes_freed: i64,
    /// Files the policy covered that could not be removed. A later sweep tries again.
    failed: Vec<String>,
}

/// Delete audio the policy no longer covers.
async fn sweep_audio(State(state): State<Shared>) -> ApiResult<Json<SweepBody>> {
    let db = state.db().await;
    let policy = notewise_storage::retention_policy(&db);
    let report = notewise_storage::sweep(&db, policy, chrono::Utc::now())?;

    if !report.failed.is_empty() {
        tracing::warn!(
            count = report.failed.len(),
            "some retained audio could not be deleted; a later sweep will retry"
        );
    }

    Ok(Json(SweepBody {
        deleted: report.deleted,
        bytes_freed: report.bytes_freed,
        failed: report.failed,
    }))
}

#[derive(Debug, Serialize)]
struct TemplateBody {
    id: String,
    name: String,
    prompt: String,
    /// Seeded, and therefore not deletable. The UI hides the delete control rather than offering
    /// one that always fails.
    is_builtin: bool,
}

impl From<notewise_storage::SummaryTemplate> for TemplateBody {
    fn from(t: notewise_storage::SummaryTemplate) -> Self {
        Self {
            id: t.id.to_string(),
            name: t.name,
            prompt: t.prompt,
            is_builtin: t.is_builtin,
        }
    }
}

async fn list_templates(State(state): State<Shared>) -> ApiResult<Json<Vec<TemplateBody>>> {
    let db = state.db().await;
    let templates = notewise_storage::SummaryRepository::new(&db).templates()?;
    Ok(Json(templates.into_iter().map(Into::into).collect()))
}

#[derive(Debug, Deserialize)]
struct TemplateInput {
    name: String,
    prompt: String,
}

async fn create_template(
    State(state): State<Shared>,
    Json(body): Json<TemplateInput>,
) -> ApiResult<Json<TemplateBody>> {
    let (name, prompt) = validated_template(&body)?;
    let db = state.db().await;
    let made = notewise_storage::SummaryRepository::new(&db)
        .create_template(notewise_storage::NewSummaryTemplate { name, prompt })?;
    Ok(Json(made.into()))
}

async fn update_template(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<TemplateInput>,
) -> ApiResult<Json<TemplateBody>> {
    let id = parse_storage_id(&id)?;
    let (name, prompt) = validated_template(&body)?;
    let db = state.db().await;
    let updated =
        notewise_storage::SummaryRepository::new(&db).update_template(id, &name, &prompt)?;
    Ok(Json(updated.into()))
}

async fn delete_template(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let id = parse_storage_id(&id)?;
    let db = state.db().await;
    notewise_storage::SummaryRepository::new(&db).delete_template(id)?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// An empty name or prompt is refused here rather than stored.
///
/// A template with no prompt would summarise with an empty instruction, which is not an error the
/// model reports — it just returns something worse, and the user has no way to tell why.
fn validated_template(body: &TemplateInput) -> ApiResult<(String, String)> {
    let name = body.name.trim();
    let prompt = body.prompt.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("a template needs a name".into()));
    }
    if prompt.is_empty() {
        return Err(ApiError::BadRequest(
            "a template needs a prompt; an empty one summarises with no instruction at all".into(),
        ));
    }
    Ok((name.to_string(), prompt.to_string()))
}

#[derive(Debug, Deserialize)]
struct TitleInput {
    title: String,
}

async fn set_title(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<TitleInput>,
) -> ApiResult<Json<serde_json::Value>> {
    let id = parse_storage_id(&id)?;
    let title = body.title.trim();
    if title.is_empty() {
        return Err(ApiError::BadRequest(
            "a meeting needs a title; an untitled meeting is unfindable".into(),
        ));
    }

    let db = state.db().await;
    let meeting = notewise_storage::MeetingRepository::new(&db).set_title(id, title)?;
    Ok(Json(serde_json::json!({ "title": meeting.title })))
}

#[derive(Debug, Deserialize)]
struct SegmentTextInput {
    text: String,
}

/// Correct a mis-transcribed line.
///
/// Empty is refused: deleting a line by blanking it would leave a gap in the transcript with no
/// record that anything was there, which is a different operation from correcting one.
async fn set_segment_text(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<SegmentTextInput>,
) -> ApiResult<Json<serde_json::Value>> {
    let id = parse_storage_id(&id)?;
    let text = body.text.trim();
    if text.is_empty() {
        return Err(ApiError::BadRequest(
            "a transcript line cannot be emptied; correct it or leave it".into(),
        ));
    }

    let db = state.db().await;
    notewise_storage::MeetingRepository::new(&db).set_segment_text(id, text)?;
    Ok(Json(serde_json::json!({ "text": text })))
}

fn parse_storage_id(raw: &str) -> ApiResult<notewise_storage::Id> {
    raw.parse()
        .map_err(|_| ApiError::BadRequest(format!("'{raw}' is not an id")))
}

#[derive(Debug, Serialize)]
struct PendingNotification {
    id: String,
    /// What triggered it, matching `graph::NodeKind` naming — `meeting`, `action_item`, and so on.
    ///
    /// A `Notification` has no title field, and inventing one here would mean this endpoint
    /// deciding wording that belongs to whatever displays it. The kind plus the body is what the
    /// row actually holds.
    source_kind: String,
    source_id: String,
    body: String,
    created_at: String,
}

/// Desktop notifications waiting to be shown.
///
/// # Why the engine does not deliver them itself
///
/// `NotificationRepository` has had `pending_on` and `mark_delivered` since the comms layer
/// landed, and nothing ever drained them. It could not: the engine has no way to raise an OS
/// notification, and `apps/desktop/src-tauri` is excluded from the workspace on purpose so engine
/// CI never pulls a GUI toolchain.
///
/// So the split is the same one `connector_outbox` already uses — the engine decides *that*
/// something should be delivered, and the surface that can actually deliver it drains the queue
/// and says so. Here that surface is the frontend, using the browser notification API, which works
/// in the Tauri webview and in a browser. That also makes it testable, which a Tauri-only plugin
/// would not have been.
async fn pending_notifications(
    State(state): State<Shared>,
) -> ApiResult<Json<Vec<PendingNotification>>> {
    let db = state.db().await;
    let pending = notewise_storage::NotificationRepository::new(&db)
        .pending_on(notewise_storage::NotificationChannel::Desktop)?;

    Ok(Json(
        pending
            .into_iter()
            .map(|n| PendingNotification {
                id: n.id.to_string(),
                source_kind: n.source_kind,
                source_id: n.source_id.to_string(),
                body: n.body,
                created_at: n.created_at.to_rfc3339(),
            })
            .collect(),
    ))
}

/// Record that a notification was actually shown.
///
/// Called by whoever displayed it, not by whoever queued it. A row marked delivered before anything
/// appeared would be a queue that silently drops things — the failure mode hardest to notice,
/// because the evidence is the absence of a notification nobody was expecting.
async fn mark_delivered(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let id: notewise_storage::Id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("'{id}' is not an id")))?;

    let db = state.db().await;
    notewise_storage::NotificationRepository::new(&db).mark_delivered(id, chrono::Utc::now())?;

    Ok(Json(serde_json::json!({ "delivered": true })))
}

#[derive(Debug, Deserialize)]
struct MergeBody {
    /// The workspace to fold in.
    from: String,
    /// Report what would move and change nothing. Defaults to **true**, so a caller that forgets
    /// the field previews rather than mutates.
    #[serde(default = "default_true")]
    dry_run: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
struct MergeResponse {
    applied: bool,
    summary: String,
    meetings: usize,
    transcript_segments: usize,
    notes: usize,
    people_added: usize,
    people_merged: usize,
    skipped_conflicts: usize,
}

/// Fold another workspace into this one, or report what that would move.
///
/// `dry_run` defaults to true. This is the only endpoint here that changes a user's data in a way
/// nothing else undoes, and a caller that omits a field should get the harmless behaviour — the
/// opposite default would make a forgotten parameter destructive.
async fn merge_workspace(
    State(state): State<Shared>,
    Json(body): Json<MergeBody>,
) -> ApiResult<Json<MergeResponse>> {
    let mode = if body.dry_run {
        MergeMode::Preview
    } else {
        MergeMode::Apply
    };
    let source = std::path::PathBuf::from(&body.from);

    let report = {
        let db = state.db().await;
        notewise_storage::merge_from(&db, &source, mode)?
    };

    Ok(Json(MergeResponse {
        applied: !body.dry_run,
        summary: report.summary(),
        meetings: report.meetings,
        transcript_segments: report.transcript_segments,
        notes: report.notes,
        people_added: report.people_added,
        people_merged: report.people_merged,
        skipped_conflicts: report.skipped_conflicts,
    }))
}

#[derive(Debug, Serialize)]
struct RulesResponse {
    rules: Vec<StoredRoute>,
    /// Names in evaluation order, as the *running* router holds them. A rule that failed to build
    /// is stored but absent here, which is how a user sees that one is not in force.
    active: Vec<String>,
}

/// The stored rule set, and which of them the running router actually built.
///
/// Reporting both matters: a rule whose backend could not be constructed is skipped at load with a
/// warning, and a settings page showing only the stored list would present it as working.
async fn get_rules(State(state): State<Shared>) -> ApiResult<Json<RulesResponse>> {
    let rules = {
        let db = state.db().await;
        crate::state::stored_routes(&db)
    };

    Ok(Json(RulesResponse {
        rules,
        active: state.ai().route_names(),
    }))
}

#[derive(Debug, Deserialize)]
struct PutRulesBody {
    rules: Vec<StoredRoute>,
}

/// Replace the rule set.
///
/// Validated before it is stored, and the validation is the point. Two mistakes are easy to make
/// and impossible to notice afterwards, because both produce a rule that is listed and never runs:
/// a rule below a catch-all, and a rule whose bounds contradict each other. Rejecting them here
/// means the failure arrives while the user is looking at the rule they just wrote.
async fn put_rules(
    State(state): State<Shared>,
    Json(body): Json<PutRulesBody>,
) -> ApiResult<Json<RulesResponse>> {
    let specs: Vec<RouteSpec> = body.rules.iter().map(|r| r.spec.clone()).collect();

    for (i, rule) in body.rules.iter().enumerate() {
        if rule.spec.name.trim().is_empty() {
            return Err(ApiError::BadRequest(format!(
                "rule {i} has no name; a rule you cannot refer to is a rule you cannot debug"
            )));
        }
        if !rule.backend.is_selectable() {
            return Err(ApiError::BadRequest(format!(
                "rule '{}' targets '{}', which does not run a model and would return invented \
                 answers",
                rule.spec.name,
                rule.backend.as_str()
            )));
        }
        if rule.backend.requires_endpoint() && rule.endpoint.is_none() {
            return Err(ApiError::BadRequest(format!(
                "rule '{}' targets {} and needs an endpoint URL",
                rule.spec.name,
                rule.backend.label()
            )));
        }
    }

    if let Some(i) = unreachable_route(&specs) {
        return Err(ApiError::BadRequest(format!(
            "rule '{}' can never run: an earlier rule matches every request. Move it above that \
             rule, or give that rule a condition.",
            specs[i].name
        )));
    }

    if let Some(i) = contradictory_route(&specs) {
        return Err(ApiError::BadRequest(format!(
            "rule '{}' can never match: its size bounds cannot both hold",
            specs[i].name
        )));
    }

    let encoded = serde_json::to_string(&body.rules)
        .map_err(|e| ApiError::Internal(format!("could not encode the rules: {e}")))?;

    {
        let db = state.db().await;
        SettingsRepository::new(&db)
            .set(ROUTING_RULES_KEY, &encoded)
            .map_err(|e| ApiError::Internal(format!("could not save the rules: {e}")))?;
    }

    // Rebuild the live router so the change applies without a restart. Reusing the current kind
    // and model means this is a policy change and never a silent backend change.
    let ai = state.ai();
    state
        .switch_backend(ai.kind(), Some(ai.model_id().to_string()), None)
        .await?;

    Ok(Json(RulesResponse {
        rules: body.rules,
        active: state.ai().route_names(),
    }))
}

#[derive(Debug, Deserialize)]
struct ExplainBody {
    /// Which kind of work to simulate. Defaults to a summary, the expensive case.
    #[serde(default)]
    task: Option<String>,
    /// Roughly how large the input is. Defaults to something small.
    #[serde(default)]
    estimated_tokens: Option<usize>,
    /// Title or question text, for keyword rules.
    #[serde(default)]
    text: Option<String>,
    /// Local hour to simulate, 0..=23. Defaults to now.
    #[serde(default)]
    hour_of_day: Option<u8>,
}

#[derive(Debug, Serialize)]
struct ExplainResponse {
    /// Human-readable: which rule matched and which provider it reaches.
    decision: String,
    /// The facts the decision was made from, echoed so a surprising answer is debuggable.
    task: String,
    estimated_tokens: usize,
    hour_of_day: u8,
}

/// Where a request with these characteristics would go, and why.
///
/// A dry run. Nothing is sent to any provider — the whole point is to answer the cost question
/// without incurring the cost.
async fn explain(
    State(state): State<Shared>,
    Json(body): Json<ExplainBody>,
) -> ApiResult<Json<ExplainResponse>> {
    let task = match body.task.as_deref().map(str::trim) {
        None | Some("") => TaskKind::Summarize,
        Some(name) => TaskKind::parse(name).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "unknown task '{name}'; expected one of summarize, extract_decisions, \
                 extract_action_items, chat"
            ))
        })?,
    };

    if let Some(hour) = body.hour_of_day {
        if hour > 23 {
            return Err(ApiError::BadRequest(format!(
                "hour_of_day must be 0..=23, got {hour}"
            )));
        }
    }

    let text = body.text.unwrap_or_default();
    let hour_of_day = body.hour_of_day.unwrap_or_else(current_hour);
    let facts = RequestFacts {
        task,
        estimated_tokens: body.estimated_tokens.unwrap_or(0),
        hour_of_day,
        text: text.to_lowercase(),
        // A dry run does not probe. Spending a health check to answer a hypothetical would make
        // the explain endpoint the one place that touches the network to describe what *might*
        // happen, and a rule gated on health simply reports as not matching.
        local_healthy: None,
    };

    Ok(Json(ExplainResponse {
        decision: state.ai().explain(&facts),
        task: task.as_str().to_string(),
        estimated_tokens: facts.estimated_tokens,
        hour_of_day,
    }))
}

fn current_hour() -> u8 {
    use chrono::Timelike;
    chrono::Local::now().hour() as u8
}

#[derive(Debug, Deserialize)]
struct InstallDefaultBody {
    /// The backend to send heavy work to. Must be a real, selectable backend.
    quality_backend: String,
    #[serde(default)]
    quality_model: Option<String>,
}

/// Install the two-rule starting policy: heavy work to a chosen backend, everything else local.
///
/// Offered as an action rather than seeded automatically. Seeding it at first launch would mean a
/// fresh install silently acquiring a rule that sends transcripts to a provider the user has not
/// chosen — and with no cloud backend configured the rules would collapse to local anyway, so it
/// would be a no-op that only confuses the settings page.
async fn install_default(
    State(state): State<Shared>,
    Json(body): Json<InstallDefaultBody>,
) -> ApiResult<Json<RulesResponse>> {
    let backend = BackendKind::parse(body.quality_backend.trim()).ok_or_else(|| {
        ApiError::BadRequest(format!("unknown backend '{}'", body.quality_backend))
    })?;

    if !backend.is_selectable() {
        return Err(ApiError::BadRequest(format!(
            "'{}' does not run a model",
            backend.as_str()
        )));
    }

    let rules = vec![StoredRoute {
        spec: RouteSpec {
            name: "Heavy work".into(),
            when: vec![Predicate::Task(vec![TaskKind::Summarize])],
        },
        backend,
        model: body.quality_model,
        endpoint: None,
        redaction: Default::default(),
    }];

    // One rule, not two. The spec described "summaries to quality, everything else local", and the
    // second half is what the default backend already does — expressing it as a catch-all rule
    // would add something that can never change the outcome and would make every later rule
    // unreachable.
    put_rules(State(state), Json(PutRulesBody { rules })).await
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
            .expect("request")
    }

    fn send(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    }

    fn rule(name: &str, when: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "name": name, "when": when, "backend": "ollama" })
    }

    /// The safe default matters more than the convenience here: a caller that forgets `dry_run`
    /// must preview, not mutate.
    /// A queue nothing drains is the state this endpoint exists to end, so the round trip that
    /// matters is: queued shows up, delivered stops showing up.
    #[tokio::test]
    async fn a_queued_desktop_notification_is_offered_then_stops_being_offered() {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ));
        let app = routes().with_state(Arc::clone(&state));

        let id = {
            let db = state.db().await;
            notewise_storage::NotificationRepository::new(&db)
                .create(notewise_storage::NewNotification {
                    source_kind: "meeting".into(),
                    source_id: notewise_storage::Id::new(),
                    recipient: "me".into(),
                    channel: notewise_storage::NotificationChannel::Desktop,
                    body: "your standup is starting".into(),
                })
                .expect("queue")
                .id
        };

        let (status, body) = call(&app, get("/v1/notifications/pending")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body.as_array().expect("array").len(), 1, "{body}");
        assert_eq!(body[0]["body"], "your standup is starting");
        assert_eq!(body[0]["source_kind"], "meeting");

        let (status, _) = call(
            &app,
            send(
                "POST",
                &format!("/v1/notifications/{id}/delivered"),
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (_, after) = call(&app, get("/v1/notifications/pending")).await;
        assert_eq!(
            after.as_array().expect("array").len(),
            0,
            "a delivered notification must not be offered again: {after}"
        );
    }

    #[tokio::test]
    async fn only_desktop_notifications_are_offered() {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ));
        let app = routes().with_state(Arc::clone(&state));

        {
            let db = state.db().await;
            // Slack and email have no delivery path in this product. Offering them to a frontend
            // that can only raise a desktop notification would mark them delivered when nothing
            // reached anyone.
            for channel in [
                notewise_storage::NotificationChannel::Slack,
                notewise_storage::NotificationChannel::Email,
            ] {
                notewise_storage::NotificationRepository::new(&db)
                    .create(notewise_storage::NewNotification {
                        source_kind: "meeting".into(),
                        source_id: notewise_storage::Id::new(),
                        recipient: "me".into(),
                        channel,
                        body: "elsewhere".into(),
                    })
                    .expect("queue");
            }
        }

        let (_, body) = call(&app, get("/v1/notifications/pending")).await;
        assert_eq!(body.as_array().expect("array").len(), 0, "{body}");
    }

    #[tokio::test]
    async fn a_malformed_notification_id_is_a_client_error() {
        let (status, _) = call(
            &app(),
            send(
                "POST",
                "/v1/notifications/not-an-id/delivered",
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn merge_defaults_to_a_dry_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("other.db");
        notewise_storage::Database::open(&source).expect("source");

        let (status, body) = call(
            &app(),
            send(
                "POST",
                "/v1/workspace/merge",
                serde_json::json!({ "from": source.to_str().unwrap() }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["applied"], false,
            "omitting dry_run must not apply a merge"
        );
    }

    #[tokio::test]
    async fn merging_a_missing_workspace_is_a_client_error() {
        let (status, body) = call(
            &app(),
            send(
                "POST",
                "/v1/workspace/merge",
                serde_json::json!({ "from": "/nope/missing.db", "dry_run": true }),
            ),
        )
        .await;
        // Named "a client error", so it checks for one. `assert_ne!(status, OK)` also passes on a
        // 500, which is the distinction this test exists to make.
        assert!(status.is_client_error(), "{status} {body}");
    }

    /// Range parsing is the only part of seeking that can be subtly wrong, so it is tested directly
    /// rather than through a player.
    #[test]
    fn byte_ranges_are_parsed_the_way_players_send_them() {
        let total = 1000;

        assert_eq!(parse_byte_range("bytes=0-499", total), Some((0, 499)));
        // Open-ended: from here to the end, which is what a player sends to stream on.
        assert_eq!(parse_byte_range("bytes=500-", total), Some((500, 999)));
        // Suffix: the last N bytes.
        assert_eq!(parse_byte_range("bytes=-100", total), Some((900, 999)));
        // Past the end is clamped, not refused — every browser asks for this on the last chunk.
        assert_eq!(parse_byte_range("bytes=900-5000", total), Some((900, 999)));
        assert_eq!(parse_byte_range("bytes=0-0", total), Some((0, 0)));

        // Nonsense, and the cases this deliberately declines.
        assert_eq!(parse_byte_range("bytes=1000-1001", total), None);
        assert_eq!(parse_byte_range("bytes=500-100", total), None);
        assert_eq!(parse_byte_range("bytes=abc-def", total), None);
        assert_eq!(parse_byte_range("items=0-10", total), None);
        assert_eq!(parse_byte_range("bytes=-0", total), None);
        // Multi-range: browsers do not send it for media, and half-answering is worse than not.
        assert_eq!(parse_byte_range("bytes=0-10,20-30", total), None);
        // An empty file has no satisfiable range.
        assert_eq!(parse_byte_range("bytes=0-10", 0), None);
    }

    #[tokio::test]
    async fn asking_for_audio_that_was_never_kept_is_a_404() {
        let id = notewise_storage::Id::new();
        let (status, _) = call(&app(), get(&format!("/v1/meetings/{id}/audio"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// The pointer outliving the file reads as "no audio" rather than an error, because that is what
    /// it means to the user and the next sweep tidies the row.
    #[tokio::test]
    async fn a_pointer_to_a_missing_file_is_a_404() {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ));
        let app = routes().with_state(Arc::clone(&state));

        let id = {
            let db = state.db().await;
            let meeting = notewise_storage::MeetingRepository::new(&db)
                .create(notewise_storage::NewMeeting {
                    project_id: None,
                    title: "Standup".into(),
                    source: notewise_storage::MeetingSource::Microphone,
                    started_at: chrono::Utc::now(),
                })
                .expect("meeting");
            notewise_storage::set_audio(&db, meeting.id, "/nowhere/gone.wav", 100).expect("attach");
            meeting.id
        };

        let (status, _) = call(&app, get(&format!("/v1/meetings/{id}/audio"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn nothing_is_remembered_on_a_fresh_engine() {
        let (status, body) = call(&app(), get("/v1/memories")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["memories"].as_array().expect("array").len(), 0);
        assert_eq!(body["global_used"], 0);
        assert_eq!(body["global_cap"], notewise_storage::GLOBAL_CAP);
    }

    #[tokio::test]
    async fn a_memory_can_be_added_edited_and_deleted() {
        let app = app();
        let (status, made) = call(
            &app,
            send(
                "POST",
                "/v1/memories",
                serde_json::json!({"text": "I prefer short summaries"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{made}");
        assert_eq!(made["scope"], "global");
        assert_eq!(
            made["origin"], "manual",
            "a user has to be able to tell what they wrote from what was inferred"
        );

        let id = made["id"].as_str().expect("id");
        let (status, edited) = call(
            &app,
            send(
                "PUT",
                &format!("/v1/memories/{id}"),
                serde_json::json!({"text": "I prefer very short summaries"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(edited["text"], "I prefer very short summaries");
        assert_eq!(edited["id"], made["id"], "editing must not replace it");

        let (status, _) = call(
            &app,
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/memories/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (_, after) = call(&app, get("/v1/memories")).await;
        assert_eq!(after["memories"].as_array().expect("array").len(), 0);
    }

    /// The cap arrives as a refusal with a sentence, because a cap the user cannot see is a cap
    /// that arrives as a surprise.
    #[tokio::test]
    async fn the_global_cap_is_refused_at_the_boundary() {
        let app = app();
        for n in 0..notewise_storage::GLOBAL_CAP {
            let (status, _) = call(
                &app,
                send(
                    "POST",
                    "/v1/memories",
                    serde_json::json!({"text": format!("I have preference number {n}")}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }

        let (status, body) = call(
            &app,
            send(
                "POST",
                "/v1/memories",
                serde_json::json!({"text": "I have one preference too many"}),
            ),
        )
        .await;
        // A cap being reached is a rule, not a fault: 409, and never logged as a server error.
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.to_string().contains("Delete one"), "{body}");
    }

    #[tokio::test]
    async fn an_unknown_scope_is_refused() {
        let (status, _) = call(
            &app(),
            send(
                "POST",
                "/v1/memories",
                serde_json::json!({"text": "I prefer short summaries", "scope": "everywhere"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn an_empty_memory_is_refused() {
        let (status, _) = call(
            &app(),
            send("POST", "/v1/memories", serde_json::json!({"text": "   "})),
        )
        .await;
        assert_ne!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn audio_retention_is_off_until_someone_turns_it_on() {
        let (status, body) = call(&app(), get("/v1/audio/retention")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["policy"], "off");
        assert_eq!(body["retained"], 0);
        assert_eq!(
            body["can_enable"], true,
            "an unencrypted workspace can retain audio"
        );
    }

    #[tokio::test]
    async fn a_retention_policy_round_trips() {
        let app = app();
        for (input, expected) in [("days:7", "days:7"), ("until_deleted", "until_deleted")] {
            let (status, body) = call(
                &app,
                send(
                    "PUT",
                    "/v1/audio/retention",
                    serde_json::json!({ "policy": input }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["policy"], expected);
        }
    }

    /// A garbled value must never be read as permission to keep recordings, so it is refused here
    /// rather than silently parsed as `off` and stored.
    #[tokio::test]
    async fn an_unrecognised_retention_policy_is_refused() {
        for bad in ["forever", "days:0", "yes", ""] {
            let (status, _) = call(
                &app(),
                send(
                    "PUT",
                    "/v1/audio/retention",
                    serde_json::json!({ "policy": bad }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
        }
    }

    /// `off` is a real policy and must be accepted — it is how a user turns the feature back off.
    #[tokio::test]
    async fn off_is_accepted_and_sweeps() {
        let app = app();
        let (status, body) = call(
            &app,
            send(
                "PUT",
                "/v1/audio/retention",
                serde_json::json!({ "policy": "off" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["policy"], "off");
        assert_eq!(body["retained"], 0);
    }

    #[tokio::test]
    async fn a_sweep_with_nothing_retained_reports_nothing() {
        let (status, body) = call(
            &app(),
            send("POST", "/v1/audio/sweep", serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["deleted"], 0);
        assert_eq!(body["bytes_freed"], 0);
    }

    #[tokio::test]
    async fn the_builtin_templates_are_listed() {
        let (status, body) = call(&app(), get("/v1/summary-templates")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.as_array().expect("array").len(), 3, "{body}");
        assert_eq!(body[0]["is_builtin"], true);
    }

    #[tokio::test]
    async fn a_template_round_trips_and_can_be_edited_then_deleted() {
        let app = app();
        let (status, made) = call(
            &app,
            send(
                "POST",
                "/v1/summary-templates",
                serde_json::json!({ "name": "Mine", "prompt": "decisions only" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{made}");
        let id = made["id"].as_str().expect("id").to_string();
        assert_eq!(made["is_builtin"], false);

        let (status, edited) = call(
            &app,
            send(
                "PUT",
                &format!("/v1/summary-templates/{id}"),
                serde_json::json!({ "name": "Mine", "prompt": "decisions and owners" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(edited["prompt"], "decisions and owners");

        let (status, _) = call(
            &app,
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/summary-templates/{id}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (_, after) = call(&app, get("/v1/summary-templates")).await;
        assert_eq!(after.as_array().expect("array").len(), 3);
    }

    /// An empty prompt is not an error the model reports — it just answers worse, and the user has
    /// no way to tell why.
    #[tokio::test]
    async fn a_template_with_no_prompt_or_no_name_is_refused() {
        let app = app();
        for body in [
            serde_json::json!({ "name": "x", "prompt": "   " }),
            serde_json::json!({ "name": "  ", "prompt": "y" }),
        ] {
            let (status, _) = call(&app, send("POST", "/v1/summary-templates", body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]
    async fn a_builtin_cannot_be_deleted_over_http() {
        let app = app();
        let (_, listed) = call(&app, get("/v1/summary-templates")).await;
        let id = listed[0]["id"].as_str().expect("id").to_string();

        let (status, body) = call(
            &app,
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/summary-templates/{id}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await;

        // The status, not just "not OK". `assert_ne!(status, OK)` is what this said, and a 500
        // satisfies it — which is exactly what the endpoint returned until `StorageError::Refused`
        // was mapped. A refusal the product intends has to be a 4xx, or callers cannot tell a rule
        // from an outage and the message gets logged as a server fault.
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "refused", "{body}");
        // The rule, in words, and not a table name.
        assert!(
            body["error"]
                .as_str()
                .expect("a reason")
                .contains("built-in"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn an_empty_title_or_transcript_line_is_refused() {
        let app = app();
        let id = notewise_storage::Id::new();

        let (status, _) = call(
            &app,
            send(
                "PUT",
                &format!("/v1/meetings/{id}/title"),
                serde_json::json!({ "title": "   " }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = call(
            &app,
            send(
                "PUT",
                &format!("/v1/segments/{id}/text"),
                serde_json::json!({ "text": "" }),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "blanking a line is a different operation from correcting one"
        );
    }

    #[tokio::test]
    async fn a_fresh_engine_has_no_rules() {
        let (status, body) = call(&app(), get("/v1/routing/rules")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["rules"].as_array().expect("rules").len(), 0);
        assert_eq!(body["active"].as_array().expect("active").len(), 0);
    }

    #[tokio::test]
    async fn rules_round_trip_and_become_active() {
        let app = app();
        let rules = serde_json::json!({
            "rules": [rule("summaries", serde_json::json!([{ "task": ["summarize"] }]))]
        });

        let (status, body) = call(&app, send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["active"][0], "summaries");

        let (_, reread) = call(&app, get("/v1/routing/rules")).await;
        assert_eq!(reread["rules"][0]["name"], "summaries");
    }

    /// The two mistakes that produce a rule which is listed and never runs. Both have to be caught
    /// while the user is still looking at the rule they wrote.
    #[tokio::test]
    async fn a_rule_below_a_catch_all_is_refused() {
        let rules = serde_json::json!({
            "rules": [
                rule("everything", serde_json::json!([])),
                rule("never runs", serde_json::json!([{ "task": ["chat"] }])),
            ]
        });

        let (status, body) = call(&app(), send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body.to_string().contains("never runs"),
            "the error must name the dead rule: {body}"
        );
    }

    #[tokio::test]
    async fn a_rule_with_impossible_bounds_is_refused() {
        let rules = serde_json::json!({
            "rules": [rule(
                "impossible",
                serde_json::json!([
                    { "input_tokens_over": 1000 },
                    { "input_tokens_under": 100 },
                ])
            )]
        });

        let (status, body) = call(&app(), send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    #[tokio::test]
    async fn an_unnamed_rule_is_refused() {
        let rules = serde_json::json!({ "rules": [rule("   ", serde_json::json!([]))] });
        let (status, _) = call(&app(), send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// The mock backend answers every request with fixed text. Routing to it would produce
    /// invented summaries of a real meeting, presented exactly like real ones.
    #[tokio::test]
    async fn a_rule_targeting_the_mock_backend_is_refused() {
        let rules = serde_json::json!({
            "rules": [{ "name": "fake", "when": [], "backend": "mock" }]
        });
        let (status, body) = call(&app(), send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    #[tokio::test]
    async fn a_custom_endpoint_backend_without_a_url_is_refused() {
        let rules = serde_json::json!({
            "rules": [{ "name": "custom", "when": [], "backend": "openai_compatible" }]
        });
        let (status, body) = call(&app(), send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    #[tokio::test]
    async fn explain_names_the_default_when_nothing_matches() {
        let (status, body) = call(
            &app(),
            send("POST", "/v1/routing/explain", serde_json::json!({})),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(
            body["decision"]
                .as_str()
                .expect("decision")
                .contains("default"),
            "{body}"
        );
        assert_eq!(body["task"], "summarize", "a summary is the expensive case");
    }

    #[tokio::test]
    async fn explain_names_the_rule_that_would_match() {
        let app = app();
        let rules = serde_json::json!({
            "rules": [rule("big ones", serde_json::json!([{ "input_tokens_over": 100 }]))]
        });
        let (status, _) = call(&app, send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::OK);

        let (_, matched) = call(
            &app,
            send(
                "POST",
                "/v1/routing/explain",
                serde_json::json!({ "estimated_tokens": 5000 }),
            ),
        )
        .await;
        assert!(
            matched["decision"]
                .as_str()
                .expect("decision")
                .contains("big ones"),
            "{matched}"
        );

        let (_, small) = call(
            &app,
            send(
                "POST",
                "/v1/routing/explain",
                serde_json::json!({ "estimated_tokens": 10 }),
            ),
        )
        .await;
        assert!(
            small["decision"]
                .as_str()
                .expect("decision")
                .contains("default"),
            "{small}"
        );
    }

    #[tokio::test]
    async fn explain_rejects_an_unknown_task_and_an_impossible_hour() {
        let app = app();

        let (status, _) = call(
            &app,
            send(
                "POST",
                "/v1/routing/explain",
                serde_json::json!({ "task": "transcribe" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = call(
            &app,
            send(
                "POST",
                "/v1/routing/explain",
                serde_json::json!({ "hour_of_day": 25 }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_default_policy_installs_one_rule_for_heavy_work() {
        let (status, body) = call(
            &app(),
            send(
                "POST",
                "/v1/routing/default",
                serde_json::json!({ "quality_backend": "ollama" }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["rules"].as_array().expect("rules").len(), 1);
        assert_eq!(body["active"][0], "Heavy work");
    }

    #[tokio::test]
    async fn the_default_policy_refuses_a_backend_that_runs_no_model() {
        let (status, _) = call(
            &app(),
            send(
                "POST",
                "/v1/routing/default",
                serde_json::json!({ "quality_backend": "mock" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
