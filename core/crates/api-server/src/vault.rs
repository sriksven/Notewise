//! Mirroring a meeting to a markdown vault, and settling a file the user edited.
//!
//! # Two gaps this closes
//!
//! **Nothing recorded a divergence.** `vault.rs` has refused to overwrite an edited file since
//! `b6e9c3f`, correctly, and told nobody — so a user who annotated a meeting note in Obsidian got a
//! mirror that quietly stopped updating and no way to find out why. The dispatcher records it now;
//! this is where it is answered.
//!
//! **Nothing ever pushed.** The vault sink had no producer at all: the only `enqueue` calls in the
//! repository were in tests. A conflict-resolution screen for a mirror that never ran would be
//! furniture, so mirroring is a request here.
//!
//! # Why a push is synchronous even though a background drain exists
//!
//! [`crate::sync`] drains the outbox on a timer, so a delivery deferred by a transient failure is
//! retried without anybody asking. Mirroring still drains in the same request, because the user
//! pressed a button and the answer they want is what happened — including "that file has your edits
//! in it, here are three things you can do". Waiting up to half a minute to be told that is a worse
//! screen than one that says it immediately.
//!
//! The two are safe together: the outbox leases a claimed row, so a background pass and a button
//! press cannot deliver the same thing twice.
//!
//! # How "overwrite" is implemented, and why it is not a file write
//!
//! By adopting the file's *current* content as the baseline and pushing normally. The sink then sees
//! a file matching what it last recorded and writes over it, which means the filesystem work, the
//! iCloud-lock handling, and the fingerprint bookkeeping all stay in the one place that already does
//! them. A second writer here would be a second chance to get the promise wrong.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router as AxumRouter};
use notewise_connectors::{vault_fingerprint, Dispatcher, RetryPolicy, VaultSink};
use notewise_graph::{EdgeKind, Graph, NodeKind, NodeRef};
use notewise_storage::{
    meeting_to_markdown, DocumentRepository, ExportOptions, Id, NewExternalItem, NewNote,
    NewOutboxEntry, NoteRepository, Resolution,
};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

type Shared = Arc<AppState>;

pub fn routes() -> AxumRouter<Shared> {
    AxumRouter::new()
        .route("/v1/meetings/:id/mirror", post(mirror_meeting))
        .route("/v1/vault/divergences", get(list_divergences))
        .route("/v1/vault/divergences/:id/resolve", post(resolve))
}

// ---------------------------------------------------------------- mirroring

#[derive(Debug, Serialize)]
struct MirrorResult {
    /// `written`, `diverged`, or `unavailable`.
    outcome: &'static str,
    /// The file, when one was written or refused.
    path: Option<String>,
    /// The divergence to answer, when the write was refused.
    divergence_id: Option<String>,
    message: String,
}

/// Write this meeting to the vault.
///
/// Refuses rather than overwrites when the file has been edited, and says which divergence to
/// answer — the point being that a refusal is now actionable instead of silent.
async fn mirror_meeting(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<MirrorResult>> {
    let meeting_id: Id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("'{id}' is not a meeting id")))?;

    if state.connectors().sink(VaultSink::ID).is_err() {
        return Ok(Json(MirrorResult {
            outcome: "unavailable",
            path: None,
            divergence_id: None,
            message: "No vault folder is connected. Choose one in Connectors first.".into(),
        }));
    }

    let (title, markdown) = {
        let db = state.db().await;
        let meeting = notewise_storage::MeetingRepository::new(&db).get(meeting_id)?;
        (
            meeting.title,
            meeting_to_markdown(&db, meeting_id, ExportOptions::default())?,
        )
    };

    push(&state, meeting_id, &title, &markdown).await
}

/// Enqueue one push and drain it.
///
/// # Why the idempotency key is unique per request
///
/// It was the render's fingerprint, which is wrong here and wrong in an instructive way: mirroring
/// the same meeting twice reused the completed row and delivered nothing, so a file the user had
/// edited in between was never *attempted* — and a divergence is only discoverable by attempting.
/// The mirror looked like it worked and told nobody anything.
///
/// The key's job is to stop a *retry* duplicating a delivery, not to stop a person asking twice. For
/// a file mirror asking twice is harmless — the same bytes to the same path — and it is the only way
/// to notice the file changed underneath.
///
/// # Why this runs on its own thread with its own connection
///
/// `Dispatcher::drain` is async and borrows the database, and `Database` is `Send` but not `Sync` —
/// so a future holding the shared guard across the drain's awaits is not `Send` and cannot be an
/// axum handler. The same problem the recording pipeline has, solved the same way: a dedicated
/// connection on a thread of its own, rather than holding the app's one across a filesystem write
/// that an iCloud sync client can stall for seconds.
///
/// The cost is that mirroring needs a workspace on disk. An `--ephemeral` engine reports that rather
/// than failing obscurely.
async fn push(
    state: &Shared,
    meeting_id: Id,
    title: &str,
    markdown: &str,
) -> ApiResult<Json<MirrorResult>> {
    let registry = state.connectors();
    let payload = serde_json::json!({ "title": title, "markdown": markdown }).to_string();
    let key = format!("{}:{meeting_id}:{}", VaultSink::ID, Id::new());

    let outcome = crate::sync::on_a_worker(state, move |db, runtime| {
        notewise_storage::OutboxRepository::new(db)
            .enqueue(NewOutboxEntry {
                connector_id: VaultSink::ID.to_string(),
                node_kind: "meeting".to_string(),
                node_id: meeting_id,
                operation: notewise_connectors::Operation::Update.as_str().to_string(),
                payload,
                idempotency_key: key,
            })
            .map_err(|e| e.to_string())?;

        let dispatcher = Dispatcher::new(registry, RetryPolicy::default());
        let report = runtime
            .block_on(dispatcher.drain(db))
            .map_err(|e| e.to_string())?;

        // The divergence for *this* meeting, found through the graph rather than by matching the
        // path. The vault names a file from the title plus twelve hex characters of the id, so a
        // path never contains the whole id — a filter on it silently matched nothing, which is how
        // this reported "the vault could not be written to" for a perfectly ordinary conflict.
        let diverged = vault_item_for(db, meeting_id)
            .and_then(|item_id| {
                DocumentRepository::new(db)
                    .divergence_for(item_id)
                    .ok()
                    .flatten()
            })
            .filter(|divergence| divergence.resolved_at.is_none());

        Ok(PushOutcome {
            delivered: report.delivered,
            diverged,
        })
    })
    .await;

    // An in-memory engine cannot mirror, and says so rather than failing obscurely.
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(reason) if reason.contains("in memory") => {
            return Ok(Json(MirrorResult {
                outcome: "unavailable",
                path: None,
                divergence_id: None,
                message: "Mirroring needs a workspace stored on disk; this engine is in memory \
                          only."
                    .into(),
            }))
        }
        Err(reason) => return Err(ApiError::Internal(reason)),
    };

    if outcome.delivered > 0 {
        return Ok(Json(MirrorResult {
            outcome: "written",
            path: None,
            divergence_id: None,
            message: "Written to your vault.".into(),
        }));
    }

    // Refused, or something else went wrong. A divergence is the interesting case and the only one
    // the user can act on.
    match outcome.diverged {
        Some(divergence) => Ok(Json(MirrorResult {
            outcome: "diverged",
            path: Some(divergence.path.clone()),
            divergence_id: Some(divergence.id.to_string()),
            message: format!(
                "{} has been edited outside Notewise. Nothing was overwritten.",
                divergence.path
            ),
        })),
        None => Ok(Json(MirrorResult {
            outcome: "unavailable",
            path: None,
            divergence_id: None,
            message: "The vault could not be written to. Check the folder still exists.".into(),
        })),
    }
}

/// What one drain produced.
#[derive(Debug)]
struct PushOutcome {
    delivered: u32,
    /// The unsettled divergence for the meeting that was pushed, if the write was refused.
    diverged: Option<notewise_storage::Divergence>,
}

/// The vault's external item for a meeting, by its `SyncedTo` edge.
///
/// The same walk the dispatcher makes to find an existing reference. Duplicated rather than exposed
/// from the connector crate: it is four lines, and a public "find the item for this node" on a crate
/// whose whole job is not knowing about the graph would be the wrong seam to widen.
fn vault_item_for(db: &notewise_storage::Database, meeting_id: Id) -> Option<Id> {
    let related = Graph::new(db)
        .related(NodeRef::new(NodeKind::Meeting, meeting_id), 1)
        .ok()?;

    let items = notewise_storage::ExternalItemRepository::new(db);

    related.into_iter().find_map(|edge| {
        if edge.node.kind != NodeKind::ExternalItem || edge.via != EdgeKind::SyncedTo {
            return None;
        }
        let item = items.get(edge.node.id).ok()?;
        (item.connector_id == VaultSink::ID).then_some(item.id)
    })
}

// ---------------------------------------------------------------- divergences

#[derive(Debug, Serialize)]
struct DivergenceBody {
    id: String,
    path: String,
    /// The file's name, which is what a person recognises.
    file_name: String,
    detected_at: String,
    /// The meeting this file mirrors, when the link is still there.
    meeting_id: Option<String>,
    meeting_title: Option<String>,
    /// What the user wrote, so the choice is made looking at it rather than in the abstract.
    ///
    /// `None` when the file cannot be read — which is also a reason a divergence exists, since an
    /// unreadable file is treated as edited.
    current_content: Option<String>,
}

/// How much of the edited file to show.
const PREVIEW_CHARS: usize = 4_000;

/// Vault files edited outside Notewise and not yet answered.
async fn list_divergences(State(state): State<Shared>) -> ApiResult<Json<Vec<DivergenceBody>>> {
    let divergences = {
        let db = state.db().await;
        DocumentRepository::new(&db).open_divergences()?
    };

    let mut out = Vec::with_capacity(divergences.len());

    for divergence in divergences {
        let (meeting_id, meeting_title) = meeting_for(&state, divergence.external_item_id).await;

        // Read here rather than in the interface: the frontend has no filesystem, and a decision
        // about somebody's writing should be made while looking at it.
        let current_content = tokio::fs::read_to_string(&divergence.path)
            .await
            .ok()
            .map(|text| text.chars().take(PREVIEW_CHARS).collect());

        out.push(DivergenceBody {
            id: divergence.id.to_string(),
            file_name: std::path::Path::new(&divergence.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| divergence.path.clone()),
            path: divergence.path,
            detected_at: divergence.detected_at.to_rfc3339(),
            meeting_id: meeting_id.map(|id| id.to_string()),
            meeting_title,
            current_content,
        });
    }

    Ok(Json(out))
}

/// The meeting an external item mirrors, by its `SyncedTo` edge.
async fn meeting_for(state: &Shared, external_item_id: Id) -> (Option<Id>, Option<String>) {
    let db = state.db().await;

    let Ok(related) =
        Graph::new(&db).related(NodeRef::new(NodeKind::ExternalItem, external_item_id), 1)
    else {
        return (None, None);
    };

    for edge in related {
        if edge.node.kind != NodeKind::Meeting || edge.via != EdgeKind::SyncedTo {
            continue;
        }
        let title = notewise_storage::MeetingRepository::new(&db)
            .get(edge.node.id)
            .ok()
            .map(|meeting| meeting.title);
        return (Some(edge.node.id), title);
    }

    (None, None)
}

#[derive(Debug, Deserialize)]
struct ResolveBody {
    /// `keep`, `overwrite`, or `copy_to_note`.
    resolution: String,
}

#[derive(Debug, Serialize)]
struct Resolved {
    resolution: &'static str,
    /// The note created, when the user's writing was kept as one.
    note_id: Option<String>,
    /// Whether mirroring resumed, and what happened when it did.
    mirror: Option<MirrorResult>,
    message: String,
}

/// Settle a divergence.
///
/// Three choices, and each does something different to the file:
///
/// - **keep** leaves it alone and stops mirroring that meeting. Nothing is written, now or later.
/// - **overwrite** replaces it with the current render.
/// - **copy_to_note** saves what the user wrote as a note linked to the meeting, *then* overwrites.
///   The one that loses nothing, and the least obvious of the three — which is why the response says
///   where the copy went.
async fn resolve(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<ResolveBody>,
) -> ApiResult<Json<Resolved>> {
    let divergence_id: Id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("'{id}' is not a divergence id")))?;

    let resolution = match body.resolution.trim() {
        "keep" => Resolution::Kept,
        "overwrite" => Resolution::Overwritten,
        "copy_to_note" => Resolution::CopiedToNote,
        other => {
            return Err(ApiError::BadRequest(format!(
                "'{other}' is not a resolution; use keep, overwrite, or copy_to_note"
            )))
        }
    };

    let divergence = {
        let db = state.db().await;
        DocumentRepository::new(&db)
            .open_divergences()?
            .into_iter()
            .find(|d| d.id == divergence_id)
            .ok_or_else(|| ApiError::NotFound("that divergence is already settled".into()))?
    };

    let (meeting_id, meeting_title) = meeting_for(&state, divergence.external_item_id).await;

    // Read before anything is changed. Copying the user's writing into a note after overwriting the
    // file would copy our own render.
    let edited = tokio::fs::read_to_string(&divergence.path).await.ok();

    let mut note_id = None;

    if resolution == Resolution::CopiedToNote {
        let Some(edited) = edited.as_deref().filter(|text| !text.trim().is_empty()) else {
            return Err(ApiError::Conflict(
                "that file cannot be read, so there is nothing to copy into a note. Choose keep or \
                 overwrite instead."
                    .into(),
            ));
        };

        let db = state.db().await;
        let note = NoteRepository::new(&db).create(NewNote {
            project_id: None,
            title: format!(
                "Your notes on {}",
                meeting_title
                    .as_deref()
                    .unwrap_or(&divergence.file_name_of())
            ),
            body: edited.to_string(),
        })?;

        // Linked, so the note is findable from the meeting rather than only from a list.
        if let Some(meeting_id) = meeting_id {
            let _ = Graph::new(&db).connect(
                NodeRef::new(NodeKind::Note, note.id),
                EdgeKind::References,
                NodeRef::new(NodeKind::Meeting, meeting_id),
            );
        }

        note_id = Some(note.id.to_string());
    }

    // Adopt the file as the baseline, for the two resolutions that resume mirroring. See the module
    // docs for why this is not a file write.
    if resolution != Resolution::Kept {
        let Some(edited) = edited.as_deref() else {
            return Err(ApiError::Conflict(
                "that file cannot be read, so it cannot be safely overwritten. Move it aside and \
                 mirror the meeting again."
                    .into(),
            ));
        };

        let db = state.db().await;
        let item =
            notewise_storage::ExternalItemRepository::new(&db).get(divergence.external_item_id)?;

        notewise_storage::ExternalItemRepository::new(&db).upsert(NewExternalItem {
            connector_id: item.connector_id,
            external_id: item.external_id,
            url: item.url,
            title: item.title,
            remote_version: Some(vault_fingerprint(edited)),
        })?;
    }

    {
        let db = state.db().await;
        DocumentRepository::new(&db).resolve_divergence(divergence_id, resolution)?;
    }

    // Now push, for the resolutions that asked for it.
    let mirror = match (resolution, meeting_id) {
        (Resolution::Kept, _) | (_, None) => None,
        (_, Some(meeting_id)) => {
            let (title, markdown) = {
                let db = state.db().await;
                let meeting = notewise_storage::MeetingRepository::new(&db).get(meeting_id)?;
                (
                    meeting.title,
                    meeting_to_markdown(&db, meeting_id, ExportOptions::default())?,
                )
            };
            Some(push(&state, meeting_id, &title, &markdown).await?.0)
        }
    };

    Ok(Json(Resolved {
        resolution: resolution.as_str(),
        note_id: note_id.clone(),
        mirror,
        message: match resolution {
            Resolution::Kept => {
                "Your file is untouched, and Notewise will stop mirroring this meeting to it."
                    .into()
            }
            Resolution::Overwritten => "The file has been replaced with the current notes.".into(),
            Resolution::CopiedToNote => {
                "What you wrote is saved as a note, and the file has been refreshed.".into()
            }
        },
    }))
}

/// The file's name, for a heading.
trait FileName {
    fn file_name_of(&self) -> String;
}

impl FileName for notewise_storage::Divergence {
    fn file_name_of(&self) -> String {
        std::path::Path::new(&self.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.clone())
    }
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

    /// A file-backed workspace with a vault connected, because mirroring needs both.
    async fn state_with_vault() -> (Shared, tempfile::TempDir, tempfile::TempDir) {
        let workspace = tempfile::tempdir().expect("workspace dir");
        let vault = tempfile::tempdir().expect("vault dir");

        let db = Database::open(workspace.path().join("notewise.db")).expect("a workspace");
        let state = AppState::new(
            db,
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        );

        let mut registry = notewise_connectors::ConnectorRegistry::new();
        registry.register_sink(Arc::new(VaultSink::new(vault.path())));
        state.set_connectors(registry);

        (Arc::new(state), workspace, vault)
    }

    async fn seed_meeting(state: &Shared) -> Id {
        let db = state.db().await;
        let repo = notewise_storage::MeetingRepository::new(&db);
        let meeting = repo
            .create(notewise_storage::NewMeeting {
                project_id: None,
                title: "Platform standup".into(),
                source: notewise_storage::MeetingSource::Import,
                started_at: chrono::Utc::now(),
            })
            .expect("a meeting");
        repo.add_segment(notewise_storage::NewTranscriptSegment {
            meeting_id: meeting.id,
            speaker: Some("Alex".into()),
            text: "we agreed to ship on Friday".into(),
            start_ms: 0,
            end_ms: 2_000,
            confidence: None,
        })
        .expect("a segment");
        repo.end(meeting.id, chrono::Utc::now()).expect("ends");
        meeting.id
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

    fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("builds")
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("builds")
    }

    /// The one file in the vault.
    fn vault_file(dir: &std::path::Path) -> std::path::PathBuf {
        std::fs::read_dir(dir)
            .expect("reads the vault")
            .flatten()
            .map(|entry| entry.path())
            .next()
            .expect("the vault has a file in it")
    }

    /// The producer that did not exist: a meeting reaching a vault at all.
    #[tokio::test]
    async fn a_meeting_can_be_written_to_the_vault() {
        let (state, _workspace, vault) = state_with_vault().await;
        let app = routes().with_state(Arc::clone(&state));
        let meeting_id = seed_meeting(&state).await;

        let (status, body) = call(
            &app,
            post(
                &format!("/v1/meetings/{meeting_id}/mirror"),
                serde_json::json!({}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["outcome"], "written", "{body}");

        let written = std::fs::read_to_string(vault_file(vault.path())).expect("reads");
        assert!(written.contains("Platform standup"), "{written}");
        assert!(written.contains("ship on Friday"), "{written}");
    }

    /// With no vault connected the answer says so rather than failing obscurely.
    #[tokio::test]
    async fn mirroring_without_a_vault_says_to_connect_one() {
        let workspace = tempfile::tempdir().expect("dir");
        let state = Arc::new(AppState::new(
            Database::open(workspace.path().join("notewise.db")).expect("a workspace"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ));
        let app = routes().with_state(Arc::clone(&state));
        let meeting_id = seed_meeting(&state).await;

        let (status, body) = call(
            &app,
            post(
                &format!("/v1/meetings/{meeting_id}/mirror"),
                serde_json::json!({}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["outcome"], "unavailable");
        assert!(body["message"]
            .as_str()
            .expect("a message")
            .contains("Connectors"));
    }

    /// The whole point: an edited file is not overwritten, and the user is told which file and
    /// offered the choice.
    #[tokio::test]
    async fn an_edited_file_diverges_instead_of_being_overwritten() {
        let (state, _workspace, vault) = state_with_vault().await;
        let app = routes().with_state(Arc::clone(&state));
        let meeting_id = seed_meeting(&state).await;

        call(
            &app,
            post(
                &format!("/v1/meetings/{meeting_id}/mirror"),
                serde_json::json!({}),
            ),
        )
        .await;

        let path = vault_file(vault.path());
        std::fs::write(&path, "# Platform standup\n\nmy own thoughts on this").expect("edits");

        let (_, body) = call(
            &app,
            post(
                &format!("/v1/meetings/{meeting_id}/mirror"),
                serde_json::json!({}),
            ),
        )
        .await;

        assert_eq!(body["outcome"], "diverged", "{body}");
        assert!(body["divergence_id"].is_string(), "{body}");

        // The edit survived.
        let after = std::fs::read_to_string(&path).expect("reads");
        assert!(after.contains("my own thoughts"), "{after}");

        // And it is listed, with the content to decide about.
        let (status, listed) = call(&app, get("/v1/vault/divergences")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed.as_array().expect("a list").len(), 1);
        assert_eq!(listed[0]["meeting_title"], "Platform standup");
        assert!(listed[0]["current_content"]
            .as_str()
            .expect("the content")
            .contains("my own thoughts"));
    }

    /// Keep: nothing is written, now or later.
    #[tokio::test]
    async fn keeping_the_file_leaves_it_alone_and_stops_mirroring() {
        let (state, _workspace, vault) = state_with_vault().await;
        let app = routes().with_state(Arc::clone(&state));
        let meeting_id = seed_meeting(&state).await;

        call(
            &app,
            post(
                &format!("/v1/meetings/{meeting_id}/mirror"),
                serde_json::json!({}),
            ),
        )
        .await;
        let path = vault_file(vault.path());
        std::fs::write(&path, "mine now").expect("edits");
        let (_, diverged) = call(
            &app,
            post(
                &format!("/v1/meetings/{meeting_id}/mirror"),
                serde_json::json!({}),
            ),
        )
        .await;
        let id = diverged["divergence_id"].as_str().expect("an id");

        let (status, resolved) = call(
            &app,
            post(
                &format!("/v1/vault/divergences/{id}/resolve"),
                serde_json::json!({ "resolution": "keep" }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{resolved}");
        assert_eq!(resolved["resolution"], "kept");
        assert_eq!(
            resolved["mirror"],
            serde_json::Value::Null,
            "nothing was pushed"
        );
        assert_eq!(std::fs::read_to_string(&path).expect("reads"), "mine now");

        // Answered, so it is off the list — and a later attempt does not put it back, which is what
        // "stop mirroring this" means.
        assert!(call(&app, get("/v1/vault/divergences"))
            .await
            .1
            .as_array()
            .expect("a list")
            .is_empty());
    }

    /// Overwrite: the file becomes the current render.
    #[tokio::test]
    async fn overwriting_replaces_the_file_with_the_current_notes() {
        let (state, _workspace, vault) = state_with_vault().await;
        let app = routes().with_state(Arc::clone(&state));
        let meeting_id = seed_meeting(&state).await;

        call(
            &app,
            post(
                &format!("/v1/meetings/{meeting_id}/mirror"),
                serde_json::json!({}),
            ),
        )
        .await;
        let path = vault_file(vault.path());
        std::fs::write(&path, "scribbles").expect("edits");
        let (_, diverged) = call(
            &app,
            post(
                &format!("/v1/meetings/{meeting_id}/mirror"),
                serde_json::json!({}),
            ),
        )
        .await;
        let id = diverged["divergence_id"].as_str().expect("an id");

        let (status, resolved) = call(
            &app,
            post(
                &format!("/v1/vault/divergences/{id}/resolve"),
                serde_json::json!({ "resolution": "overwrite" }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{resolved}");
        assert_eq!(resolved["mirror"]["outcome"], "written", "{resolved}");

        let after = std::fs::read_to_string(&path).expect("reads");
        assert!(after.contains("ship on Friday"), "{after}");
        assert!(!after.contains("scribbles"), "{after}");
    }

    /// Copy to note: the writing survives as a note, and the mirror resumes. The one resolution that
    /// loses nothing, and the least obvious of the three.
    #[tokio::test]
    async fn copying_to_a_note_keeps_the_writing_and_resumes_mirroring() {
        let (state, _workspace, vault) = state_with_vault().await;
        let app = routes().with_state(Arc::clone(&state));
        let meeting_id = seed_meeting(&state).await;

        call(
            &app,
            post(
                &format!("/v1/meetings/{meeting_id}/mirror"),
                serde_json::json!({}),
            ),
        )
        .await;
        let path = vault_file(vault.path());
        std::fs::write(&path, "the thing I actually took away from this call").expect("edits");
        let (_, diverged) = call(
            &app,
            post(
                &format!("/v1/meetings/{meeting_id}/mirror"),
                serde_json::json!({}),
            ),
        )
        .await;
        let id = diverged["divergence_id"].as_str().expect("an id");

        let (status, resolved) = call(
            &app,
            post(
                &format!("/v1/vault/divergences/{id}/resolve"),
                serde_json::json!({ "resolution": "copy_to_note" }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{resolved}");
        assert!(resolved["note_id"].is_string(), "{resolved}");
        assert_eq!(resolved["mirror"]["outcome"], "written", "{resolved}");

        // The writing is a note, findable from the meeting.
        let db = state.db().await;
        let notes = NoteRepository::new(&db).list_recent(10).expect("reads");
        let note = notes
            .iter()
            .find(|n| n.body.contains("actually took away"))
            .expect("the writing was kept");
        assert!(note.title.contains("Platform standup"), "{}", note.title);

        let linked = Graph::new(&db)
            .related(NodeRef::new(NodeKind::Note, note.id), 1)
            .expect("reads")
            .into_iter()
            .any(|edge| edge.node.kind == NodeKind::Meeting && edge.node.id == meeting_id);
        assert!(linked, "the note must be reachable from the meeting");
        drop(db);

        // And the file is the render again.
        let after = std::fs::read_to_string(&path).expect("reads");
        assert!(after.contains("ship on Friday"), "{after}");
    }

    #[tokio::test]
    async fn an_unknown_resolution_is_refused() {
        let (state, _workspace, _vault) = state_with_vault().await;
        let app = routes().with_state(Arc::clone(&state));

        let (status, _) = call(
            &app,
            post(
                &format!("/v1/vault/divergences/{}/resolve", Id::new()),
                serde_json::json!({ "resolution": "merge" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn resolving_a_divergence_that_is_already_settled_is_a_404() {
        let (state, _workspace, _vault) = state_with_vault().await;
        let app = routes().with_state(Arc::clone(&state));

        let (status, _) = call(
            &app,
            post(
                &format!("/v1/vault/divergences/{}/resolve", Id::new()),
                serde_json::json!({ "resolution": "keep" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// An empty list is the ordinary state, not an error.
    #[tokio::test]
    async fn no_divergences_is_an_empty_list() {
        let (state, _workspace, _vault) = state_with_vault().await;
        let app = routes().with_state(Arc::clone(&state));

        let (status, body) = call(&app, get("/v1/vault/divergences")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().expect("a list").is_empty());
    }
}
