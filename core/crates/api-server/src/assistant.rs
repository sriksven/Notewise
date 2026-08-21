//! The three assistant surfaces built on the foundation: asking about the screen, acting on a
//! selection, and continuing a sentence.
//!
//! Specs 9b, 9c and 9d. Each is a separate design in the program map and each is staged after
//! dictation, which is where the shared parts were proven — the hotkey registry, the insertion
//! tiers, the screen-context reduction, the permission reads.
//!
//! # What each of them is, honestly
//!
//! **9b, asking about the screen.** The frontmost application's text becomes context for a question.
//! The hard part is not the model call; it is deciding what to include, and that decision is
//! [`notewise_os_input::ScreenContext`]'s: a selection beats the whole field, the field beats text
//! recognised from pixels, and the whole thing is capped. Nothing here sends a screenshot anywhere.
//!
//! **9c, acting on a selection.** Read what is highlighted, transform it, and either put it back or
//! hand it over. "Replace" is offered only where the target will accept a replacement, because on a
//! target that will not the user loses their selection and gets nothing.
//!
//! **9d, continuing a sentence.** Not ghost text. The accessibility API can read a field and replace
//! a selection; it cannot draw unaccepted text inside another process's view, and no amount of
//! effort here changes that. So the suggestion is shown in Notewise's own window and inserted at the
//! caret when accepted. That is a smaller feature than the design sketched and it is the one that
//! can actually exist.
//!
//! # Why none of these auto-execute
//!
//! Every one of them ends in either text on screen or text inserted where the user put their cursor.
//! None of them can reach a tool, a connector, or a file. That is the same boundary
//! [`crate::tools`] draws for external calls, applied to a surface that sees more of the user's
//! screen than anything else in the product.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router as AxumRouter};
use notewise_ai_router::{AiBackend, ChatMessage, ChatRequest};
use notewise_os_input::{
    aftermath, continuation_of, decide, CompletionPolicy, Decision, Insertion, OsInputError,
    ScreenContext, TypingActivity, PROMPT_LIMIT,
};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::retrieval;
use crate::state::AppState;

type Shared = Arc<AppState>;

pub fn routes() -> AxumRouter<Shared> {
    AxumRouter::new()
        .route("/v1/assistant/ask", post(ask_about_screen))
        .route("/v1/assistant/selection", get(read_selection))
        .route("/v1/assistant/act", post(act_on_selection))
        .route("/v1/assistant/actions", get(list_actions))
        .route("/v1/assistant/complete", post(complete))
        .route(
            "/v1/assistant/typing",
            get(typing_status).post(start_typing).delete(stop_typing),
        )
}

/// Translate a platform refusal at the boundary.
///
/// A missing grant is a 409 rather than a 403: the request was understood, the state is what makes
/// it refusable, and the fix is a switch the user can flip. A build that cannot do it at all is a
/// 501, because no amount of flipping will help.
fn refusal(error: OsInputError) -> ApiError {
    match &error {
        OsInputError::PermissionRequired { .. } => ApiError::Conflict(error.to_string()),
        OsInputError::Unsupported { .. } => ApiError::NotImplemented(error.to_string()),
        _ => ApiError::Internal(error.to_string()),
    }
}

// ---------------------------------------------------------------- 9b: asking about the screen

#[derive(Debug, Deserialize)]
struct AskBody {
    question: String,
    /// Answer from the question alone, without reading the screen.
    ///
    /// Here so the surface can be used on a machine where the grant has been refused, rather than
    /// being unavailable entirely.
    #[serde(default)]
    ignore_screen: bool,
}

#[derive(Debug, Serialize)]
struct AskAnswer {
    text: String,
    model: String,
    /// Whether there was any screen context behind the answer.
    grounded: bool,
    /// Exactly what was put in front of the model, so a user can see what left their screen.
    ///
    /// Returned in full rather than summarised. This surface reads the frontmost application, and
    /// "what did you send" has to be answerable without trusting a description of it.
    context: Option<ScreenContext>,
    context_prompt: String,
}

/// Answer a question about whatever the user is looking at.
async fn ask_about_screen(
    State(state): State<Shared>,
    Json(body): Json<AskBody>,
) -> ApiResult<Json<AskAnswer>> {
    let question = body.question.trim();
    if question.is_empty() {
        return Err(ApiError::BadRequest("ask something".into()));
    }

    let context = if body.ignore_screen {
        None
    } else {
        // A refused grant is fatal here rather than silently ignored: a user who asked about their
        // screen and got an answer from nothing has been misled about what happened.
        Some(notewise_os_input::screen_context().map_err(refusal)?)
    };

    let context_prompt = context
        .as_ref()
        .map(|context| context.to_prompt(PROMPT_LIMIT))
        .unwrap_or_default();

    let grounded = !context_prompt.trim().is_empty();

    // The same grounding rules the workspace question path uses, so an answer about the screen is
    // held to the standard as an answer about a meeting: cite the material, and say when it is not
    // there rather than filling the gap.
    let instructions = if grounded {
        format!(
            "You answer questions about what the user is looking at on their screen right now.\n\n\
             WHAT IS ON SCREEN:\n{context_prompt}\n\n{}",
            retrieval::GROUNDING_RULES
        )
    } else {
        "You answer the user's question directly. You were given no information about their \
         screen, so do not pretend to know what is on it — if the question depends on that, say so."
            .to_string()
    };

    let request =
        ChatRequest::new(vec![ChatMessage::user(question)]).with_context(vec![instructions]);
    let reply = state.ai().chat(&request).await?;

    Ok(Json(AskAnswer {
        text: reply.text,
        model: reply.model,
        grounded,
        context,
        context_prompt,
    }))
}

// ---------------------------------------------------------------- 9c: acting on a selection

/// What to do to a piece of highlighted text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Act {
    /// Same meaning, better sentences.
    Rewrite,
    Shorten,
    Expand,
    FixGrammar,
    /// Make it read as a professional message.
    Formalise,
    Translate {
        /// The target language, in the user's own words. Passed through to the model rather than
        /// validated against a list: a list would be wrong for somebody's language within a week.
        language: String,
    },
    /// Produce a summary. Does not replace the original — see [`Act::replaces`].
    Summarise,
    /// Explain what the text means. Also does not replace.
    Explain,
}

impl Act {
    /// Whether the result is a replacement for the text or a new thing about it.
    ///
    /// The distinction 9c turns on, and it is not the same question as whether the *target* is
    /// editable. An explanation of a paragraph is not a paragraph: replacing the text with it would
    /// destroy what the user selected even in a field that would happily accept the write.
    pub fn replaces(&self) -> bool {
        !matches!(self, Act::Summarise | Act::Explain)
    }

    pub fn label(&self) -> String {
        match self {
            Act::Rewrite => "Rewrite".into(),
            Act::Shorten => "Make it shorter".into(),
            Act::Expand => "Expand it".into(),
            Act::FixGrammar => "Fix spelling and grammar".into(),
            Act::Formalise => "Make it more formal".into(),
            Act::Translate { language } => format!("Translate to {language}"),
            Act::Summarise => "Summarise".into(),
            Act::Explain => "Explain it".into(),
        }
    }

    /// The instruction block.
    ///
    /// Every replacing action says "return only the text" twice over, because a model that adds
    /// "Here is the rewritten version:" has that string inserted into somebody's document — the same
    /// failure the completion path guards against, and here the guard has to be the prompt because
    /// there is no shape to check the answer against.
    pub fn prompt(&self) -> String {
        let only = "Return only the resulting text. No preamble, no explanation, no quotation \
                    marks around it, and no commentary.";

        match self {
            Act::Rewrite => format!(
                "Rewrite the text to read better. Keep its meaning, its language, and roughly its \
                 length. {only}"
            ),
            Act::Shorten => format!(
                "Rewrite the text as briefly as it can be said without losing anything that \
                 matters. Keep its language. {only}"
            ),
            Act::Expand => format!(
                "Expand the text with the detail it implies but does not say. Invent no facts — if \
                 detail would have to be made up, leave it out. Keep its language. {only}"
            ),
            Act::FixGrammar => format!(
                "Correct spelling, grammar and punctuation. Change nothing else: not the wording, \
                 not the tone, not the meaning. {only}"
            ),
            Act::Formalise => format!(
                "Rewrite the text as a professional message. Keep every fact and every request in \
                 it. Keep its language. {only}"
            ),
            Act::Translate { language } => format!(
                "Translate the text into {language}. Translate it — do not summarise, explain, or \
                 improve it. {only}"
            ),
            Act::Summarise => "Summarise the text in at most three sentences, in the language it \
                 is written in. Return only the summary."
                .to_string(),
            Act::Explain => {
                "Explain what the text means, plainly, in the language it is written \
                 in. If it contains jargon, say what the jargon means. Return only the explanation."
                    .to_string()
            }
        }
    }
}

/// Every action, for a menu.
///
/// Held as data so the list a user sees and the list the server accepts cannot drift apart — the
/// same reasoning `MUTATING_TOOLS` uses in `mcp-server`.
pub fn every_act() -> Vec<Act> {
    vec![
        Act::Rewrite,
        Act::Shorten,
        Act::Expand,
        Act::FixGrammar,
        Act::Formalise,
        Act::Summarise,
        Act::Explain,
    ]
}

#[derive(Debug, Serialize)]
struct ActionInfo {
    action: Act,
    label: String,
    /// Whether choosing this replaces the selection or produces something new.
    replaces: bool,
}

async fn list_actions() -> Json<Vec<ActionInfo>> {
    Json(
        every_act()
            .into_iter()
            .map(|action| ActionInfo {
                label: action.label(),
                replaces: action.replaces(),
                action,
            })
            .collect(),
    )
}

#[derive(Debug, Serialize)]
struct SelectionBody {
    text: Option<String>,
    /// Whether the target will accept a replacement.
    ///
    /// Reported so a menu can hide "rewrite" rather than offering it and losing the user's
    /// selection for nothing.
    replaceable: bool,
    length: usize,
}

/// What the user currently has highlighted.
async fn read_selection() -> ApiResult<Json<SelectionBody>> {
    let text = notewise_os_input::read_selection().map_err(refusal)?;
    // A separate question from whether there *is* a selection, and asked separately: a target with
    // nothing selected can still be replaceable.
    let replaceable = notewise_os_input::selection_is_replaceable().unwrap_or(false);

    Ok(Json(SelectionBody {
        length: text.as_deref().map(str::len).unwrap_or(0),
        text,
        replaceable,
    }))
}

#[derive(Debug, Deserialize)]
struct ActBody {
    action: Act,
    /// The text to act on. Read from the selection when absent.
    #[serde(default)]
    text: Option<String>,
    /// Put the result back where the text came from.
    ///
    /// Refused for an action that does not replace, and refused when the target will not take it.
    #[serde(default)]
    replace: bool,
}

#[derive(Debug, Serialize)]
struct ActResult {
    action: Act,
    /// What was acted on.
    original: String,
    result: String,
    model: String,
    /// How the result got back into the document, when it did.
    insertion: Option<Insertion>,
    note: Option<String>,
}

/// How long a piece of selected text may be.
///
/// A selection is a sentence or a paragraph. Somebody who pressed ⌘A in a long document and hit
/// "rewrite" is not asking for what they would get, and the model call would be slow and expensive
/// before failing to be useful.
const MAX_SELECTION_CHARS: usize = 8_000;

async fn act_on_selection(
    State(state): State<Shared>,
    Json(body): Json<ActBody>,
) -> ApiResult<Json<ActResult>> {
    let original = match body.text {
        Some(text) => text,
        None => notewise_os_input::read_selection()
            .map_err(refusal)?
            .ok_or_else(|| ApiError::BadRequest("nothing is selected".into()))?,
    };

    if original.trim().is_empty() {
        return Err(ApiError::BadRequest("nothing is selected".into()));
    }
    if original.chars().count() > MAX_SELECTION_CHARS {
        return Err(ApiError::BadRequest(format!(
            "that is {} characters; select at most {MAX_SELECTION_CHARS}",
            original.chars().count()
        )));
    }

    let request = ChatRequest::new(vec![ChatMessage::user(&original)])
        .with_context(vec![body.action.prompt()]);
    let reply = state.ai().chat(&request).await?;
    let result = reply.text.trim().to_string();

    if result.is_empty() {
        return Err(ApiError::Internal(
            "the model returned nothing to use".into(),
        ));
    }

    // Three reasons not to write it back, and they are different reasons.
    let (insertion, note) = if !body.replace {
        (None, None)
    } else if !body.action.replaces() {
        (
            None,
            Some(format!(
                "{} produces something new rather than a replacement, so the original is untouched.",
                body.action.label()
            )),
        )
    } else {
        match notewise_os_input::insert_at_cursor(&result) {
            Ok(outcome) => {
                let note = aftermath(&outcome);
                (Some(outcome), note)
            }
            Err(error) => (
                None,
                Some(format!("{error} The result is above — copy it from here.")),
            ),
        }
    };

    Ok(Json(ActResult {
        action: body.action,
        original,
        result,
        model: reply.model,
        insertion,
        note,
    }))
}

// ---------------------------------------------------------------- 9d: continuing a sentence

#[derive(Debug, Deserialize)]
struct CompleteBody {
    /// What has been written so far. Read from the focused field when absent.
    #[serde(default)]
    text: Option<String>,
    /// When the last suggestion was asked for, so the rate limit can be applied.
    #[serde(default)]
    last_asked_ms: Option<i64>,
    /// Override the pause and rate limits. For a settings screen that lets someone tune them.
    #[serde(default)]
    policy: Option<CompletionPolicy>,
    /// Ask regardless of the policy. What the "suggest now" key does.
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Serialize)]
struct Completion {
    /// The text to insert at the caret, already spaced correctly. `None` when there is nothing worth
    /// suggesting, which is the common case and not an error.
    suggestion: Option<String>,
    /// Why there is no suggestion. Answers "why is nothing happening", which is the question this
    /// feature generates most.
    decision: Decision,
    model: Option<String>,
    /// What the suggestion continues, so a preview can show it in place.
    text: String,
}

/// Suggest a continuation of what is being typed.
///
/// Suggests only. Nothing is inserted here — acceptance is a separate request, because a completion
/// that writes itself into a document is not a suggestion.
async fn complete(
    State(state): State<Shared>,
    Json(body): Json<CompleteBody>,
) -> ApiResult<Json<Completion>> {
    let policy = body.policy.unwrap_or_default();

    let text = match body.text {
        Some(text) => text,
        None => notewise_os_input::screen_context()
            .map_err(refusal)?
            .focused_text
            .unwrap_or_default(),
    };

    let activity = notewise_os_input::typing_activity();
    let now_ms = chrono::Utc::now().timestamp_millis();

    // Forcing skips the pause and the rate limit, and nothing else: a forced suggestion for an empty
    // field is still nothing to suggest.
    let decision = if body.force {
        decide(&policy, now_ms, Some(0), None, &text)
    } else {
        decide(
            &policy,
            now_ms,
            activity.last_keystroke_ms,
            body.last_asked_ms,
            &text,
        )
    };

    if !decision.should_ask() {
        return Ok(Json(Completion {
            suggestion: None,
            decision,
            model: None,
            text,
        }));
    }

    let request = ChatRequest::new(vec![ChatMessage::user(&text)]).with_context(vec![
        "The user is part-way through writing something. Continue it from exactly where it stops.\n\n\
         Rules:\n\
         - Return only the continuation. Do not repeat what is already written.\n\
         - Continue the sentence, do not start a new topic.\n\
         - At most one sentence.\n\
         - Match the language, tone and register of what is there.\n\
         - Invent no names, numbers, dates or facts.\n\
         - If there is no sensible continuation, return nothing at all."
            .to_string(),
    ]);

    let reply = state.ai().chat(&request).await?;

    // A model that answered *about* the text rather than continuing it suggests nothing. The
    // alternative is "Sure! Here's how I would finish that" typed into somebody's email.
    let suggestion = continuation_of(&text, &reply.text);

    Ok(Json(Completion {
        suggestion,
        decision,
        model: Some(reply.model),
        text,
    }))
}

#[derive(Debug, Serialize)]
struct TypingBody {
    activity: TypingActivity,
    /// Whether this build can watch at all.
    supported: bool,
}

async fn typing_status() -> Json<TypingBody> {
    Json(TypingBody {
        activity: notewise_os_input::typing_activity(),
        supported: notewise_os_input::SUPPORTED,
    })
}

/// Start noticing when the user pauses.
///
/// Separate from every other endpoint here and never implicit: this is the one that needs Input
/// Monitoring, the most invasive grant on the platform, and it must be something a user turns on
/// rather than something that happens because they used a different feature.
async fn start_typing() -> ApiResult<Json<TypingBody>> {
    notewise_os_input::start_typing_monitor().map_err(refusal)?;
    Ok(Json(TypingBody {
        activity: notewise_os_input::typing_activity(),
        supported: notewise_os_input::SUPPORTED,
    }))
}

async fn stop_typing() -> Json<TypingBody> {
    notewise_os_input::stop_typing_monitor();
    Json(TypingBody {
        activity: notewise_os_input::typing_activity(),
        supported: notewise_os_input::SUPPORTED,
    })
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

    // ------------------------------------------------------------ the action catalogue

    /// An explanation of a paragraph is not a paragraph. Replacing the text with it would destroy
    /// what the user selected even where the field would accept the write.
    #[test]
    fn actions_that_produce_something_new_do_not_replace() {
        assert!(!Act::Summarise.replaces());
        assert!(!Act::Explain.replaces());

        for action in [
            Act::Rewrite,
            Act::Shorten,
            Act::Expand,
            Act::FixGrammar,
            Act::Formalise,
            Act::Translate {
                language: "French".into(),
            },
        ] {
            assert!(action.replaces(), "{action:?} should replace");
        }
    }

    /// Every replacing action has to forbid preamble, or "Here is the rewritten version:" ends up
    /// in somebody's document.
    #[test]
    fn every_replacing_action_asks_for_the_text_and_nothing_else() {
        for action in every_act().into_iter().filter(Act::replaces) {
            let prompt = action.prompt();
            assert!(
                prompt.contains("Return only"),
                "{action:?} does not forbid preamble: {prompt}"
            );
            assert!(
                prompt.contains("no commentary") || prompt.contains("no explanation"),
                "{action:?}: {prompt}"
            );
        }
    }

    /// Fixing grammar must not become rewriting, or the user's voice is gone.
    #[test]
    fn fixing_grammar_forbids_changing_anything_else() {
        let prompt = Act::FixGrammar.prompt();
        assert!(prompt.contains("Change nothing else"), "{prompt}");
        assert!(prompt.contains("tone"), "{prompt}");
    }

    /// Expanding is the action most likely to invent facts, so the prompt says not to.
    #[test]
    fn expanding_forbids_inventing_detail() {
        assert!(Act::Expand.prompt().contains("Invent no facts"));
    }

    /// Translating into the user's own words for a language, not a validated list — a list would be
    /// wrong for somebody's language within a week.
    #[test]
    fn translation_carries_the_language_through() {
        let action = Act::Translate {
            language: "Brazilian Portuguese".into(),
        };
        assert!(action.prompt().contains("Brazilian Portuguese"));
        assert!(action.label().contains("Brazilian Portuguese"));
    }

    /// Held as data so the menu and the server cannot drift apart.
    #[test]
    fn every_action_has_a_label_and_a_prompt() {
        for action in every_act() {
            assert!(!action.label().is_empty(), "{action:?}");
            assert!(action.prompt().len() > 40, "{action:?}");
        }
    }

    /// The wire names are stored in requests, so a rename is a breaking change and should look like
    /// one.
    #[test]
    fn actions_round_trip_through_their_wire_names() {
        for action in every_act() {
            let json = serde_json::to_string(&action).expect("serializes");
            assert_eq!(
                serde_json::from_str::<Act>(&json).expect("deserializes"),
                action
            );
        }

        assert_eq!(
            serde_json::from_str::<Act>(r#""fix_grammar""#).expect("parses"),
            Act::FixGrammar
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

    #[tokio::test]
    async fn the_action_menu_says_which_actions_replace() {
        let (status, body) = call(&app(), get("/v1/assistant/actions")).await;
        assert_eq!(status, StatusCode::OK);

        let actions = body.as_array().expect("a list");
        assert_eq!(actions.len(), every_act().len());

        let summarise = actions
            .iter()
            .find(|a| a["action"] == "summarise")
            .expect("summarise is offered");
        assert_eq!(summarise["replaces"], false);
        assert!(!summarise["label"].as_str().expect("a label").is_empty());
    }

    /// The whole path with the text supplied, so it works without a grant: transform, and do not
    /// write it back unless asked.
    #[tokio::test]
    async fn acting_on_supplied_text_returns_a_result_and_inserts_nothing() {
        let (status, body) = call(
            &app(),
            post(
                "/v1/assistant/act",
                serde_json::json!({ "action": "rewrite", "text": "this sentence are bad" }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["original"], "this sentence are bad");
        assert!(!body["result"].as_str().expect("a result").is_empty());
        assert_eq!(
            body["insertion"],
            serde_json::Value::Null,
            "nothing was asked to be replaced, so nothing was"
        );
    }

    /// Asking to replace with something that is not a replacement is refused, and says why.
    #[tokio::test]
    async fn an_action_that_does_not_replace_will_not_replace_even_when_asked() {
        let (status, body) = call(
            &app(),
            post(
                "/v1/assistant/act",
                serde_json::json!({
                    "action": "explain",
                    "text": "the p99 regressed after the shard split",
                    "replace": true
                }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["insertion"], serde_json::Value::Null);
        let note = body["note"].as_str().expect("a note");
        assert!(note.contains("untouched"), "{note}");
    }

    #[tokio::test]
    async fn acting_on_nothing_is_a_bad_request() {
        for text in ["", "   "] {
            let (status, _) = call(
                &app(),
                post(
                    "/v1/assistant/act",
                    serde_json::json!({ "action": "rewrite", "text": text }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
    }

    /// Somebody who pressed ⌘A in a long document is not asking for what they would get.
    #[tokio::test]
    async fn an_enormous_selection_is_refused_before_the_model_is_called() {
        let (status, body) = call(
            &app(),
            post(
                "/v1/assistant/act",
                serde_json::json!({ "action": "rewrite", "text": "x".repeat(20_000) }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"].as_str().expect("a reason").contains("8000"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn an_unknown_action_is_refused() {
        let (status, _) = call(
            &app(),
            post(
                "/v1/assistant/act",
                serde_json::json!({ "action": "delete_everything", "text": "hello" }),
            ),
        )
        .await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
            "{status}"
        );
    }

    /// 9b with the screen deliberately left out — which is how the surface stays usable on a
    /// machine where the grant was refused.
    #[tokio::test]
    async fn a_question_can_be_answered_without_reading_the_screen() {
        let (status, body) = call(
            &app(),
            post(
                "/v1/assistant/ask",
                serde_json::json!({ "question": "what is a shard split?", "ignore_screen": true }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["grounded"], false);
        assert_eq!(body["context"], serde_json::Value::Null);
        assert!(!body["text"].as_str().expect("an answer").is_empty());
    }

    #[tokio::test]
    async fn an_empty_question_is_a_bad_request() {
        let (status, _) = call(
            &app(),
            post("/v1/assistant/ask", serde_json::json!({ "question": "  " })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// A user who asked about their screen and got an answer from nothing has been misled, so a
    /// refused grant fails the request rather than being quietly ignored.
    #[tokio::test]
    async fn asking_about_the_screen_without_the_grant_says_so_rather_than_answering_anyway() {
        let (status, body) = call(
            &app(),
            post(
                "/v1/assistant/ask",
                serde_json::json!({ "question": "what am I looking at?" }),
            ),
        )
        .await;

        if notewise_os_input::SUPPORTED {
            // With the platform layer in, the answer depends on a grant this test cannot hold.
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
    }

    /// The common case for completion is "not yet", and it is not an error.
    #[tokio::test]
    async fn a_field_with_too_little_in_it_suggests_nothing_and_says_why() {
        let (status, body) = call(
            &app(),
            post(
                "/v1/assistant/complete",
                serde_json::json!({ "text": "hi", "force": true }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["suggestion"], serde_json::Value::Null);
        assert_eq!(body["decision"], "too_short");
        assert_eq!(
            body["model"],
            serde_json::Value::Null,
            "no model was called"
        );
    }

    /// Forcing skips the pause and the rate limit — which is what a "suggest now" key does — and
    /// nothing else.
    #[tokio::test]
    async fn forcing_a_suggestion_skips_the_pause_but_still_needs_something_to_continue() {
        let (status, body) = call(
            &app(),
            post(
                "/v1/assistant/complete",
                serde_json::json!({
                    "text": "The quarterly numbers came in lower than",
                    "last_asked_ms": chrono::Utc::now().timestamp_millis(),
                    "force": true
                }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["decision"], "ask",
            "the rate limit should have been skipped: {body}"
        );
        assert!(body["model"].is_string(), "the model was called: {body}");
    }

    /// Without forcing and with nothing typed, there is nothing to continue.
    #[tokio::test]
    async fn nothing_typed_means_nothing_to_continue() {
        let (status, body) = call(
            &app(),
            post(
                "/v1/assistant/complete",
                serde_json::json!({ "text": "The quarterly numbers came in lower than" }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        // No keystroke monitor is running in a test, so there is no pause to have finished.
        assert_eq!(body["decision"], "idle");
        assert_eq!(body["suggestion"], serde_json::Value::Null);
    }

    /// The typing monitor's state is process-global, so this is one test rather than two.
    ///
    /// Split across two, they raced: whichever started the monitor first made the other's "it is
    /// off" assertion false. That is not a flaw in either assertion — it is the shape of a resource
    /// there is exactly one of, and the fix is to check the sequence in order.
    #[tokio::test]
    async fn the_typing_monitor_is_off_until_started_and_says_why_when_it_cannot_start() {
        let _guard = typing_lock().lock().await;
        let app = app();

        // Establish the precondition rather than assuming it: another test in this process may have
        // started the monitor, and on a machine where Input Monitoring *is* granted it succeeded.
        notewise_os_input::stop_typing_monitor();
        for _ in 0..50 {
            if !notewise_os_input::typing_activity().running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let (status, body) = call(&app, get("/v1/assistant/typing")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["supported"], notewise_os_input::SUPPORTED);
        assert_eq!(body["activity"]["running"], false, "{body}");

        // Never started implicitly: this needs the most invasive grant on the platform, and that
        // must be something a user turns on rather than a side effect of using something else.
        let (status, body) = call(&app, post("/v1/assistant/typing", serde_json::json!({}))).await;

        match status {
            StatusCode::OK => {
                // Input Monitoring is granted to whatever is running these tests.
                assert_eq!(body["activity"]["running"], true, "{body}");
                let (_, stopped) = call(
                    &app,
                    Request::builder()
                        .method("DELETE")
                        .uri("/v1/assistant/typing")
                        .body(Body::empty())
                        .expect("builds"),
                )
                .await;
                assert_eq!(stopped["activity"]["running"], false, "{stopped}");
            }
            StatusCode::CONFLICT => {
                let error = body["error"].as_str().expect("a reason");
                assert!(error.contains("Input Monitoring"), "{error}");
                assert!(error.contains("System Settings"), "{error}");
            }
            StatusCode::NOT_IMPLEMENTED => {
                assert!(body["error"]
                    .as_str()
                    .expect("a reason")
                    .contains("os-input"));
            }
            other => panic!("unexpected status {other}: {body}"),
        }
    }

    /// Serialises the tests that touch the one keystroke monitor this process has.
    fn typing_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        &LOCK
    }
}
