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

/// What retrieval searches for: the latest question, or — when it only makes sense through the
/// one before it — both.
///
/// Searching on the latest question alone is right when it changes the subject ("and what about
/// headcount?"), and is why earlier turns are not pooled. It fails a follow-up that points back:
/// "Who owns that?" has one search term, "owns", which matches nothing, so after "What did we
/// decide about the search cluster?" the answer was "Nothing in this workspace matches that
/// question" — measured on `llama3.1:8b`, against a meeting that said who owns it. A short
/// question that refers back with a pronoun is searched together with the question it refers to.
fn retrieval_query(messages: &[Turn]) -> Option<String> {
    let mut asked = messages.iter().rev().filter(|m| m.role != "assistant");
    let latest = asked.next()?.content.as_str();

    let refers_back = retrieval::terms(latest).len() <= 2
        && latest
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| REFERRING_WORDS.contains(&word.to_lowercase().as_str()));

    Some(match asked.next() {
        Some(previous) if refers_back => format!("{} {latest}", previous.content),
        _ => latest.to_string(),
    })
}

/// Words that point at something said earlier rather than naming it.
const REFERRING_WORDS: &[&str] = &[
    "it", "its", "that", "this", "those", "these", "they", "them", "their", "he", "him", "his",
    "she", "her", "there",
];

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

    let question = retrieval_query(&body.messages)
        .ok_or_else(|| ApiError::BadRequest("no question in this conversation".into()))?;

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

        vec![Passage {
            kind: "note",
            id: note.id,
            title: note.title,
            text: note.body,
        }]
    }; // lock released before the model call

    let mut passages = passages;
    if wide {
        // Hybrid, so a note can be asked what the meetings said about it even when they said
        // it in different words. The database lock is taken and released inside.
        // Filtered, because a workspace-scoped answer that cites something irrelevant is
        // worse than one that says it found nothing.
        let found = retrieval::keep_relevant(
            &state,
            &question,
            retrieval::gather_hybrid(&state, &question).await?,
        )
        .await;
        // The note is already passage 1; drop it if retrieval finds it again, or the model is
        // shown the same text twice under two numbers.
        passages.extend(
            found
                .into_iter()
                .filter(|p| !(p.kind == "note" && p.id == note_id)),
        );
    }

    answer(&state, body.messages, passages, 0).await
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

    let question = retrieval_query(&body.messages)
        .ok_or_else(|| ApiError::BadRequest("no question in this conversation".into()))?;

    let retrieved = retrieval::gather_hybrid(&state, &question).await?;
    let considered = retrieved.len();
    // Similarity says which document is nearest; it does not say whether any of them is about
    // the question. See `retrieval::keep_relevant`.
    let passages = retrieval::keep_relevant(&state, &question, retrieved).await;

    // Answered here rather than by the model. Handing a model an empty context block and
    // asking it to "answer only from the material" is asking it to notice an absence, which
    // is exactly the instruction models are worst at following.
    if passages.is_empty() {
        // The two empty cases read very differently to a user, and conflating them hides the
        // one they can act on: "found nothing" means try other words, while "found six and
        // none of them answered it" means the material genuinely is not there.
        let text = if considered == 0 {
            "Nothing in this workspace matches that question. Try wording closer to what \
             would actually have been said."
                .to_string()
        } else {
            format!(
                "Looked at {considered} passage{} and none of them answer that question.",
                if considered == 1 { "" } else { "s" }
            )
        };

        return Ok(Json(serde_json::json!({
            "text": text,
            "model": state.ai().model_id(),
            "citations": [],
            "grounded": false,
            "considered": considered,
        })));
    }

    answer(&state, body.messages, passages, considered).await
}

/// Run the model against assembled passages and return the answer with its citations.
async fn answer(
    state: &Shared,
    messages: Vec<Turn>,
    passages: Vec<Passage>,
    considered: usize,
) -> ApiResult<Json<serde_json::Value>> {
    // Global memories only. A note or a workspace-wide question has no project to scope to, and
    // guessing one from whatever the retrieval happened to find would mean the same question
    // answered twice with different context depending on what matched.
    let asked = latest_question(&messages).unwrap_or_default().to_string();
    let memories = crate::memory::for_prompt(state, None, &asked).await;

    let mut context = Vec::new();
    if !memories.is_empty() {
        context.push(memories);
    }
    context.push(format!(
        "{}\n\n{}",
        retrieval::as_context(&passages),
        retrieval::GROUNDING_RULES
    ));

    let request = ChatRequest::new(to_router_messages(messages)).with_context(context);
    let response = state.ai().chat(&request).await?;

    let (text, cited) = check_references(&response.text, passages.len());
    // The passages the answer points at, under the numbers it uses. All of them when it points
    // at none — an answer that cites nothing was still written from this material, and hiding
    // the material would present it as having no source.
    let citations: Vec<serde_json::Value> = retrieval::citations(&passages)
        .into_iter()
        .filter(|citation| {
            cited.is_empty()
                || citation["n"]
                    .as_u64()
                    .is_some_and(|n| cited.contains(&(n as usize)))
        })
        .collect();

    Ok(Json(serde_json::json!({
        "text": text,
        "model": response.model,
        "citations": citations,
        // How many passages retrieval produced before the relevance filter. Reported so a
        // client can tell "found nothing" from "found six and discarded them", and so this is
        // diagnosable without attaching a debugger to a model call.
        "considered": considered.max(passages.len()),
        // Whether there was any material behind this answer at all.
        //
        // Derived from the passages rather than from whether a *search* ran. Those are not the
        // same question, and conflating them made a note-scoped answer — which is grounded on
        // the note itself and needs no search — report `false` and get labelled "nothing
        // matched" in the UI, directly under the citation it had just produced.
        "grounded": !passages.is_empty(),
    })))
}

/// Hold an answer's `[n]` references to the passages it was actually given.
///
/// Returns the text with every reference resolved, and the passage numbers it cites.
///
/// # Why the text is changed
///
/// The citation list under an answer is its legend: `[2]` in the text is the chip numbered 2.
/// Two things broke that, both measured on `llama3.1:8b`:
///
/// - **References to passages that do not exist.** Given one passage — a four-line transcript —
///   the model answered "According to [3], Raj will own the OpenSearch migration. According to
///   [4], the cutover is October 3rd", numbering the transcript's lines. The text pointed at
///   sources the user could not open. With one passage there is only one thing such a reference
///   can mean, so it becomes `[1]`; with several there is no telling which was meant, and it is
///   removed rather than guessed.
/// - **Passages listed that the answer never used.** Every retrieved passage was returned as a
///   citation, so a question about Enterprise pricing showed a chip for a 1:1 about someone's
///   medical leave beside an answer that had not used it. Only cited passages are listed now.
///
/// Brackets holding anything but numbers — a quoted `[inaudible]`, `[Dana: …]` — are not
/// references and are left alone.
fn check_references(text: &str, passages: usize) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut cited = Vec::new();
    let mut rest = text;

    while let Some(open) = rest.find('[') {
        let (before, from) = rest.split_at(open);
        out.push_str(before);
        let Some(close) = from.find(']') else {
            rest = from;
            break;
        };
        let inner = &from[1..close];
        let numbers: Option<Vec<usize>> = if !inner.trim().is_empty()
            && inner
                .chars()
                .all(|c| c.is_ascii_digit() || c == ',' || c == ' ')
        {
            inner
                .split([',', ' '])
                .filter(|piece| !piece.is_empty())
                .map(|piece| piece.parse().ok())
                .collect()
        } else {
            None
        };

        match numbers {
            Some(numbers) => {
                let mut valid: Vec<usize> = numbers
                    .into_iter()
                    .map(|n| if passages == 1 { 1 } else { n })
                    .filter(|n| (1..=passages).contains(n))
                    .collect();
                valid.dedup();
                if valid.is_empty() {
                    // The space that introduced the reference goes with it.
                    if out.ends_with(' ') {
                        out.pop();
                    }
                } else {
                    let list: Vec<String> = valid.iter().map(ToString::to_string).collect();
                    out.push('[');
                    out.push_str(&list.join(", "));
                    out.push(']');
                    cited.extend(valid);
                }
            }
            None => out.push_str(&from[..=close]),
        }
        rest = &from[close + 1..];
    }

    out.push_str(rest);
    cited.sort_unstable();
    cited.dedup();
    (out, cited)
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

    // ------------------------------------------------------------ references

    #[test]
    fn references_to_real_passages_are_kept_and_reported() {
        let (text, cited) = check_references("Three tiers [1], set by Dana [3].", 3);
        assert_eq!(text, "Three tiers [1], set by Dana [3].");
        assert_eq!(cited, vec![1, 3]);
    }

    /// Measured: one four-line passage, answered "According to [3] … According to [4] …".
    #[test]
    fn with_one_passage_every_reference_means_that_passage() {
        let (text, cited) = check_references(
            "According to [3], Raj owns it. According to [4], cutover is Oct 3.",
            1,
        );
        assert_eq!(
            text,
            "According to [1], Raj owns it. According to [1], cutover is Oct 3."
        );
        assert_eq!(cited, vec![1]);
    }

    #[test]
    fn with_several_passages_a_reference_to_none_of_them_is_removed() {
        let (text, cited) = check_references("Raj owns it [7]. Dana [2, 9] agreed.", 2);
        assert_eq!(text, "Raj owns it. Dana [2] agreed.");
        assert_eq!(cited, vec![2]);
    }

    #[test]
    fn brackets_that_are_not_references_are_left_alone() {
        let raw = "She said [inaudible] and [Dana: I'll update it]. An unclosed [1";
        let (text, cited) = check_references(raw, 2);
        assert_eq!(text, raw);
        assert!(cited.is_empty());
    }

    /// The citation list is the legend for the `[n]` in the text. A chip for a passage the answer
    /// never used — measured: a 1:1 about someone's medical leave, under an answer about pricing —
    /// is a source that is not one.
    #[tokio::test]
    async fn only_the_passages_an_answer_cites_are_listed() {
        let (app, state) = app();
        seed_meeting(&state, "Pricing review", "the pricing tiers are settled").await;
        seed_note(&state, "Pricing notes", "pricing: three tiers").await;

        // The mock backend echoes the question, so this is the answer's text: it cites [2] and a
        // passage that does not exist.
        let (status, body) = post_json(&app, "/v1/ask", ask("pricing tiers [2] [9]")).await;
        assert_eq!(status, StatusCode::OK);

        let text = body["text"].as_str().expect("text");
        assert!(!text.contains("[9]"), "{text}");
        let citations = body["citations"].as_array().expect("citations");
        assert_eq!(citations.len(), 1, "{citations:?}");
        assert_eq!(
            citations[0]["n"], 2,
            "numbered as in the text: {citations:?}"
        );
    }

    /// "Who owns that?" searches for "owns" alone, which matches nothing; measured, it was
    /// answered "Nothing in this workspace matches" right after a question it follows on from.
    #[tokio::test]
    async fn a_follow_up_that_points_back_is_searched_with_what_it_points_at() {
        let (app, state) = app();
        seed_meeting(
            &state,
            "Infrastructure sync",
            "We decided to move the search cluster to OpenSearch, and Raj will own that migration.",
        )
        .await;
        seed_meeting(&state, "Hiring", "we approved two backend headcount").await;

        let (status, body) = post_json(
            &app,
            "/v1/ask",
            serde_json::json!({"messages": [
                {"role": "user", "content": "What did we decide about the search cluster?"},
                {"role": "assistant", "content": "To move it to OpenSearch [1]."},
                {"role": "user", "content": "Who owns that?"}
            ]}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["grounded"], true, "{body}");
        let citations = body["citations"].as_array().expect("citations");
        assert!(
            citations
                .iter()
                .any(|c| c["title"] == "Infrastructure sync"),
            "{citations:?}"
        );
    }

    #[test]
    fn only_a_short_question_that_refers_back_takes_the_previous_one_along() {
        let turn = |role: &str, content: &str| Turn {
            role: role.into(),
            content: content.into(),
        };
        let thread = |last: &str| {
            vec![
                turn("user", "What did we decide about pricing?"),
                turn("assistant", "Three tiers."),
                turn("user", last),
            ]
        };

        assert_eq!(
            retrieval_query(&thread("And who owns it?")).as_deref(),
            Some("What did we decide about pricing? And who owns it?")
        );
        // A change of subject stands alone, which is why earlier turns are not pooled.
        assert_eq!(
            retrieval_query(&thread("and what about headcount?")).as_deref(),
            Some("and what about headcount?")
        );
        // A full question that happens to contain "it" stands alone too.
        assert_eq!(
            retrieval_query(&thread(
                "Did the hiring plan for it include two backend engineers?"
            ))
            .as_deref(),
            Some("Did the hiring plan for it include two backend engineers?")
        );
        assert!(retrieval_query(&[turn("assistant", "hello")]).is_none());
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
