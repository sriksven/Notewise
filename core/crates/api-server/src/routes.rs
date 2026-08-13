//! HTTP route table and handlers.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router as AxumRouter};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use notewise_ai_router::{AiBackend, TranscriptInput};
use notewise_graph::{EdgeKind, Graph, NodeKind, NodeRef};
use notewise_storage::{
    Id, Meeting, MeetingRepository, MeetingSource, NewMeeting, NewNote, NewSummary,
    NewTranscriptSegment, Note, NoteRepository, SearchRepository, SummaryRepository, Ticket,
    TicketRepository, TranscriptSegment,
};

use crate::error::{ApiError, ApiResult};
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
        .route("/v1/notes", get(list_notes).post(create_note))
        .route("/v1/tickets", get(list_tickets))
        .route("/v1/search", get(search))
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
}

async fn health(State(state): State<Shared>) -> ApiResult<Json<Health>> {
    let schema_version = state.db().await.schema_version()?;
    Ok(Json(Health {
        status: "ok",
        schema_version,
        ai_local: state.ai().is_local(),
        ai_model: state.ai().model_id().to_string(),
    }))
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

    #[tokio::test]
    async fn empty_ticket_list_is_returned_as_an_empty_array() {
        let (status, json) = call(&app(), get("/v1/tickets")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json.as_array().unwrap().len(), 0);
    }
}
