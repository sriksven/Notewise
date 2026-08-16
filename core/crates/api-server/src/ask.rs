//! Grounded question answering, over one note or over the whole workspace.
//!
//! Every answer here is assembled the same way: find the material ([`crate::retrieval`]),
//! hand the model that material and nothing else, and return the material's identity
//! alongside the answer so the user can check it.
//!
//! The returned `citations` array is not decoration. A grounded answer that cannot be traced
//! back to its source is indistinguishable from a confident invention, and the difference
//! matters more here than almost anywhere — the product's claim is that it records what was
//! actually said.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::{routing::post, Json, Router as AxumRouter};
use serde::Deserialize;

use notewise_ai_router::{AiBackend, ChatMessage, ChatRequest, Role};
use notewise_storage::{Id, NoteRepository};

use crate::error::{ApiError, ApiResult};
use crate::retrieval::{self, Passage};
use crate::state::AppState;

type Shared = Arc<AppState>;

pub(crate) fn router() -> AxumRouter<Shared> {
    AxumRouter::new()
        .route("/v1/notes/:id/chat", post(chat_about_note))
        .route("/v1/ask", post(ask_workspace))
}

#[derive(Debug, Deserialize)]
struct Turn {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct AskBody {
    messages: Vec<Turn>,
    /// `note` (default) or `workspace`.
    ///
    /// Only meaningful for the note endpoint. Ignored elsewhere rather than rejected, so a
    /// client can send one request shape to both.
    #[serde(default)]
    scope: Option<String>,
}

/// The most recent thing the user actually asked.
///
/// Retrieval runs against this rather than the whole thread: earlier turns are usually about
/// something else, and pooling them retrieves material for a question that has been answered.
fn latest_question(messages: &[Turn]) -> Option<&str> {
    messages
        .iter()
        .rev()
        .find(|m| m.role != "assistant")
        .map(|m| m.content.as_str())
}

fn to_router_messages(messages: Vec<Turn>) -> Vec<ChatMessage> {
    messages
        .into_iter()
        .map(|m| ChatMessage {
            // Anything not explicitly the assistant is the user. A client-supplied role is
            // not worth failing a request over.
            role: if m.role == "assistant" {
                Role::Assistant
            } else {
                Role::User
            },
            content: m.content,
        })
        .collect()
}

/// Ask about one note.
///
/// The note itself is always the first passage and is never dropped for budget — it is the
/// thing being asked about, and an answer assembled without it would be about something else.
/// With `scope: "workspace"` the rest of the workspace is searched too, so a note can be
/// asked what the meetings said about it.
async fn chat_about_note(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<AskBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let note_id: Id = id
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("'{id}' is not a valid id")))?;

    if body.messages.is_empty() {
        return Err(ApiError::BadRequest("messages must not be empty".into()));
    }

    let question = latest_question(&body.messages)
        .ok_or_else(|| ApiError::BadRequest("no question in this conversation".into()))?
        .to_string();

    let wide = body.scope.as_deref() == Some("workspace");

    let passages = {
        let db = state.db().await;
        let note = NoteRepository::new(&db).get(note_id)?;
        if note.deleted_at.is_some() {
            return Err(ApiError::BadRequest(
                "this note is in the trash; restore it to ask about it".into(),
            ));
        }

        if note.body.trim().is_empty() && !wide {
            return Err(ApiError::BadRequest(
                "this note is empty, so there is nothing to ground an answer in".into(),
            ));
        }

        let mut passages = vec![Passage {
            kind: "note",
            id: note.id,
            title: note.title,
            text: note.body,
        }];

        if wide {
            // The note is already passage 1; drop it if retrieval finds it again, or the
            // model is shown the same text twice under two numbers.
            passages.extend(
                retrieval::gather(&db, &question)?
                    .into_iter()
                    .filter(|p| !(p.kind == "note" && p.id == note_id)),
            );
        }

        passages
    }; // lock released before the model call

    answer(&state, body.messages, passages).await
}

/// Ask the workspace.
///
/// Nothing is pinned here: every passage comes from retrieval, so a question with no matching
/// material returns the refusal rather than a fluent answer with no basis.
async fn ask_workspace(
    State(state): State<Shared>,
    Json(body): Json<AskBody>,
) -> ApiResult<Json<serde_json::Value>> {
    if body.messages.is_empty() {
        return Err(ApiError::BadRequest("messages must not be empty".into()));
    }

    let question = latest_question(&body.messages)
        .ok_or_else(|| ApiError::BadRequest("no question in this conversation".into()))?
        .to_string();

    let passages = {
        let db = state.db().await;
        retrieval::gather(&db, &question)?
    };

    // Answered here rather than by the model. Handing a model an empty context block and
    // asking it to "answer only from the material" is asking it to notice an absence, which
    // is exactly the instruction models are worst at following.
    if passages.is_empty() {
        return Ok(Json(serde_json::json!({
            "text": "Nothing in this workspace matches that question. Search is by word, \
                     so a different wording — or a term that would actually appear in the \
                     transcript — may find it.",
            "model": state.ai().model_id(),
            "citations": [],
            "grounded": false,
        })));
    }

    answer(&state, body.messages, passages).await
}

/// Run the model against assembled passages and return the answer with its citations.
async fn answer(
    state: &Shared,
    messages: Vec<Turn>,
    passages: Vec<Passage>,
) -> ApiResult<Json<serde_json::Value>> {
    let context = vec![format!(
        "{}\n\n{}",
        retrieval::as_context(&passages),
        retrieval::GROUNDING_RULES
    )];

    let request = ChatRequest::new(to_router_messages(messages)).with_context(context);
    let response = state.ai().chat(&request).await?;

    Ok(Json(serde_json::json!({
        "text": response.text,
        "model": response.model,
        "citations": retrieval::citations(&passages),
        // Whether there was any material behind this answer at all.
        //
        // Derived from the passages rather than from whether a *search* ran. Those are not the
        // same question, and conflating them made a note-scoped answer — which is grounded on
        // the note itself and needs no search — report `false` and get labelled "nothing
        // matched" in the UI, directly under the citation it had just produced.
        "grounded": !passages.is_empty(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::TimeZone;
    use http_body_util::BodyExt;
    use notewise_ai_router::{Router as AiRouter, RouterConfig};
    use notewise_storage::{
        Database, MeetingRepository, MeetingSource, NewMeeting, NewNote, NewTranscriptSegment,
    };
    use tower::ServiceExt;

    fn app() -> (AxumRouter, Arc<AppState>) {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ));
        (router().with_state(state.clone()), state)
    }

    async fn post_json(
        app: &AxumRouter,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    fn ask(question: &str) -> serde_json::Value {
        serde_json::json!({ "messages": [{ "role": "user", "content": question }] })
    }

    async fn seed_note(state: &Arc<AppState>, title: &str, body: &str) -> Id {
        let db = state.db().await;
        NoteRepository::new(&db)
            .create(NewNote {
                project_id: None,
                title: title.into(),
                body: body.into(),
            })
            .expect("note")
            .id
    }

    async fn seed_meeting(state: &Arc<AppState>, title: &str, line: &str) {
        let db = state.db().await;
        let repo = MeetingRepository::new(&db);
        let meeting = repo
            .create(NewMeeting {
                project_id: None,
                title: title.into(),
                source: MeetingSource::Import,
                started_at: chrono::Utc
                    .timestamp_opt(1_700_000_000, 0)
                    .single()
                    .unwrap(),
            })
            .expect("meeting");
        repo.add_segment(NewTranscriptSegment {
            meeting_id: meeting.id,
            speaker: Some("Alex".into()),
            text: line.into(),
            start_ms: 0,
            end_ms: 1000,
            confidence: None,
        })
        .expect("segment");
    }

    #[tokio::test]
    async fn a_note_can_be_asked_about_itself() {
        let (app, state) = app();
        let id = seed_note(&state, "Latency", "We hold p99 under 200ms.").await;

        let (status, body) =
            post_json(&app, &format!("/v1/notes/{id}/chat"), ask("what is p99?")).await;

        assert_eq!(status, StatusCode::OK);
        assert!(!body["text"].as_str().unwrap().is_empty());

        let citations = body["citations"].as_array().expect("citations");
        assert_eq!(citations.len(), 1, "the note itself: {citations:?}");
        assert_eq!(citations[0]["kind"], "note");
        assert_eq!(citations[0]["id"], id.to_string());
        assert_eq!(citations[0]["n"], 1);
    }

    /// `grounded` reports whether there was material, not whether a search ran.
    ///
    /// The distinction is what a client keys its "nothing matched" hint off. Reporting `false`
    /// for a note-scoped answer put that hint directly beneath a citation.
    #[tokio::test]
    async fn an_answer_with_a_citation_is_always_reported_as_grounded() {
        let (app, state) = app();
        let id = seed_note(&state, "Latency", "We hold p99 under 200ms.").await;

        let (_, scoped) =
            post_json(&app, &format!("/v1/notes/{id}/chat"), ask("what is p99?")).await;
        assert_eq!(
            scoped["grounded"], true,
            "the note is material, even though nothing was searched"
        );
        assert!(!scoped["citations"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn note_scope_does_not_reach_into_the_rest_of_the_workspace() {
        let (app, state) = app();
        let id = seed_note(&state, "Latency", "We hold p99 under 200ms.").await;
        seed_meeting(&state, "Perf review", "latency is the top complaint").await;

        let (_, body) = post_json(
            &app,
            &format!("/v1/notes/{id}/chat"),
            ask("what about latency?"),
        )
        .await;

        let citations = body["citations"].as_array().expect("citations");
        assert_eq!(
            citations.len(),
            1,
            "default scope is the note alone: {citations:?}"
        );
    }

    #[tokio::test]
    async fn workspace_scope_pulls_in_the_meeting_and_keeps_the_note_first() {
        let (app, state) = app();
        let id = seed_note(&state, "Latency", "We hold p99 under 200ms.").await;
        seed_meeting(&state, "Perf review", "latency is the top complaint").await;

        let (status, body) = post_json(
            &app,
            &format!("/v1/notes/{id}/chat"),
            serde_json::json!({
                "messages": [{"role": "user", "content": "what about latency?"}],
                "scope": "workspace"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let citations = body["citations"].as_array().expect("citations");
        assert!(citations.len() >= 2, "got {citations:?}");
        assert_eq!(citations[0]["id"], id.to_string(), "the note stays first");
        assert!(
            citations.iter().any(|c| c["kind"] == "meeting"),
            "got {citations:?}"
        );
    }

    /// The note is passage 1 already. Retrieval finding it again must not list it twice — two
    /// numbers pointing at identical text makes every citation ambiguous.
    #[tokio::test]
    async fn the_note_is_never_cited_twice() {
        let (app, state) = app();
        let id = seed_note(&state, "Latency", "latency latency latency").await;

        let (_, body) = post_json(
            &app,
            &format!("/v1/notes/{id}/chat"),
            serde_json::json!({
                "messages": [{"role": "user", "content": "latency?"}],
                "scope": "workspace"
            }),
        )
        .await;

        let citations = body["citations"].as_array().expect("citations");
        let mine: Vec<_> = citations
            .iter()
            .filter(|c| c["id"] == id.to_string())
            .collect();
        assert_eq!(mine.len(), 1, "got {citations:?}");
    }

    #[tokio::test]
    async fn an_empty_note_is_refused_rather_than_answered() {
        let (app, state) = app();
        let id = seed_note(&state, "Blank", "   ").await;

        let (status, _) = post_json(&app, &format!("/v1/notes/{id}/chat"), ask("anything?")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_trashed_note_cannot_be_asked_about() {
        let (app, state) = app();
        let id = seed_note(&state, "Gone", "something").await;
        {
            let db = state.db().await;
            NoteRepository::new(&db).trash(id).expect("trash");
        }

        let (status, _) = post_json(&app, &format!("/v1/notes/{id}/chat"), ask("what?")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn asking_the_workspace_cites_what_it_found() {
        let (app, state) = app();
        seed_meeting(
            &state,
            "Pricing review",
            "we settled on three pricing tiers",
        )
        .await;

        let (status, body) =
            post_json(&app, "/v1/ask", ask("what did we decide about pricing?")).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["grounded"], true);
        let citations = body["citations"].as_array().expect("citations");
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0]["title"], "Pricing review");
    }

    /// The failure mode worth a test of its own: with no material, the model must not be
    /// asked the question at all. A fluent answer with an empty citation list is exactly what
    /// this whole module exists to prevent.
    #[tokio::test]
    async fn a_question_with_no_matching_material_is_refused_without_calling_the_model() {
        let (app, state) = app();
        seed_meeting(&state, "Pricing", "we settled on three tiers").await;

        let (status, body) =
            post_json(&app, "/v1/ask", ask("what did we say about kubernetes?")).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["grounded"], false);
        assert!(body["citations"].as_array().expect("array").is_empty());
        assert!(
            body["text"]
                .as_str()
                .unwrap()
                .contains("Nothing in this workspace"),
            "got {}",
            body["text"]
        );
    }

    #[tokio::test]
    async fn an_empty_conversation_is_a_400() {
        let (app, _state) = app();
        let (status, _) = post_json(&app, "/v1/ask", serde_json::json!({"messages": []})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_malformed_note_id_is_a_400_not_a_500() {
        let (app, _state) = app();
        let (status, _) = post_json(&app, "/v1/notes/not-an-id/chat", ask("hi")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// Retrieval must follow the conversation. A follow-up question should find material for
    /// what was just asked, not for the opening turn.
    #[tokio::test]
    async fn retrieval_follows_the_latest_question_not_the_first() {
        let (app, state) = app();
        seed_meeting(&state, "Pricing", "we settled on three pricing tiers").await;
        seed_meeting(&state, "Hiring", "we approved two backend headcount").await;

        let (_, body) = post_json(
            &app,
            "/v1/ask",
            serde_json::json!({"messages": [
                {"role": "user", "content": "what did we decide about pricing?"},
                {"role": "assistant", "content": "Three tiers."},
                {"role": "user", "content": "and what about headcount?"}
            ]}),
        )
        .await;

        let citations = body["citations"].as_array().expect("citations");
        assert_eq!(citations.len(), 1, "got {citations:?}");
        assert_eq!(citations[0]["title"], "Hiring");
    }
}
