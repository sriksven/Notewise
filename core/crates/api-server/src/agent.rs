//! An agent that works through the workspace on its own.
//!
//! # What this is modelled on
//!
//! Notion's AI agent is given a task in plain language, then searches the workspace, reads
//! what it finds, and writes a page — running for minutes across many steps rather than
//! answering in one shot. The useful part is not the model; it is that the work is *multi-step
//! and inspectable*: you can see what it looked at before you trust what it wrote.
//!
//! # What it deliberately is not
//!
//! It can do exactly two things to this workspace: search/read, and create one note. It cannot
//! edit or delete anything that already exists, cannot touch tickets, cannot reach a
//! connector, and — like every other surface in this codebase — has no path to sending an
//! email. An agent that runs unattended is the last place to widen a blast radius, and "it
//! wrote a new note you can throw away" is a failure mode a user can recover from without
//! help.
//!
//! # Why a text protocol rather than tool calling
//!
//! [`AiBackend`] has no tool-calling method, and adding one would mean implementing it for
//! every backend — including a local llama.cpp model whose tool support depends on the GGUF
//! someone downloaded. So the agent asks for one JSON action per turn and parses it. That
//! works identically on every backend, at the cost of needing to tolerate a model that wraps
//! its JSON in prose or a code fence. [`parse_action`] does tolerate exactly that, and a turn
//! that cannot be parsed at all is fed back as an observation so the model can correct itself
//! rather than the run dying.
//!
//! # Why runs live in memory
//!
//! A run is a few kilobytes of trace that is only interesting while it is happening or just
//! after. Persisting it would mean a migration, a retention policy, and a second thing to keep
//! consistent. What matters survives already: the note the agent wrote is a real note. Losing
//! the trace on restart is the right trade.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::RwLock;

use notewise_ai_router::{AiBackend, ChatMessage, ChatRequest, Role};
use notewise_storage::{Id, MeetingRepository, NewNote, NoteRepository, TicketRepository};

use crate::retrieval;

/// How many model turns a run may take before it is made to stop and summarize.
///
/// Twelve is enough for search → read a few things → write, with room to recover from a
/// mis-step. It is also a hard ceiling on cost: on a metered backend an unbounded loop is a
/// bill, and on a local one it is a laptop fan at full tilt until someone notices.
const MAX_STEPS: usize = 12;

/// How much of a document a single `read` returns.
const READ_CHARS: usize = 4_000;

/// How many finished runs to keep before evicting the oldest.
const MAX_RETAINED: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Done,
    Failed,
}

/// One turn: what the agent decided to do, and what came back.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub n: usize,
    /// The tool name, or `think` when the model produced no parseable action.
    pub action: String,
    /// The model's own one-line reason, when it gave one. Shown to the user verbatim — it is
    /// the only window into why the agent did what it did.
    pub reason: Option<String>,
    /// What the tool returned, trimmed for display.
    pub observation: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub id: Id,
    pub task: String,
    pub status: RunStatus,
    pub steps: Vec<Step>,
    /// The note the agent wrote, once it has written one.
    pub note_id: Option<Id>,
    pub note_title: Option<String>,
    /// The agent's closing message.
    pub result: Option<String>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl Run {
    fn new(task: String) -> Self {
        Self {
            id: Id::new(),
            task,
            status: RunStatus::Running,
            steps: Vec::new(),
            note_id: None,
            note_title: None,
            result: None,
            error: None,
            started_at: Utc::now(),
            finished_at: None,
        }
    }
}

/// Runs, in memory, newest last.
#[derive(Debug, Default)]
pub struct AgentRegistry {
    runs: RwLock<HashMap<Id, Run>>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    async fn insert(&self, run: Run) {
        let mut runs = self.runs.write().await;
        runs.insert(run.id, run);

        // Evict finished runs oldest-first. A run still going is never evicted, however old:
        // dropping it would leave a client polling an id that has silently ceased to exist.
        while runs.len() > MAX_RETAINED {
            let oldest = runs
                .values()
                .filter(|r| r.status != RunStatus::Running)
                .min_by_key(|r| r.started_at)
                .map(|r| r.id);
            match oldest {
                Some(id) => {
                    runs.remove(&id);
                }
                None => break,
            }
        }
    }

    async fn update(&self, id: Id, f: impl FnOnce(&mut Run)) {
        if let Some(run) = self.runs.write().await.get_mut(&id) {
            f(run);
        }
    }

    pub async fn get(&self, id: Id) -> Option<Run> {
        self.runs.read().await.get(&id).cloned()
    }

    /// Every run, newest first.
    pub async fn list(&self) -> Vec<Run> {
        let mut runs: Vec<_> = self.runs.read().await.values().cloned().collect();
        runs.sort_by_key(|run| std::cmp::Reverse(run.started_at));
        runs
    }
}

// ---------------------------------------------------------------- the protocol

/// What the agent asked to do this turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Search {
        query: String,
    },
    Read {
        kind: String,
        id: String,
    },
    RecentMeetings,
    WriteNote {
        title: String,
        body: String,
    },
    Finish {
        message: String,
    },
    /// The model said something that was not an action. Not an error — models narrate, and a
    /// run that died on the first stray sentence would be useless.
    Unparseable(String),
}

/// The instruction block. Rewritten as one string so the whole contract is visible at once.
fn system_prompt(task: &str) -> String {
    format!(
        "You are an agent working inside Notewise, a local meeting-notes app. \
You do research across the user's own meetings and notes, then write up what you found.

TASK FROM THE USER:
{task}

You work one step at a time. Reply with EXACTLY ONE JSON object and nothing else:

{{\"reason\": \"<one short line on why>\", \"action\": \"<name>\", ...arguments}}

Actions available:
  {{\"action\": \"search\", \"query\": \"<words>\"}}
      Search meetings, notes and tickets. Matching is by word, not meaning, so use words \
that would literally appear in a transcript. Returns kinds, ids and snippets.
  {{\"action\": \"read\", \"kind\": \"meeting|note|ticket\", \"id\": \"<id from a search result>\"}}
      Read one item in full.
  {{\"action\": \"recent_meetings\"}}
      List the most recent meetings, for when the task is about \"lately\" or \"this week\".
  {{\"action\": \"write_note\", \"title\": \"<title>\", \"body\": \"<markdown>\"}}
      Save your write-up as a new note. Do this once, near the end.
  {{\"action\": \"finish\", \"message\": \"<what you did, one paragraph>\"}}
      Stop. Use this when the task is done, or when the workspace does not contain what \
the task needs.

Rules:
- Search before you assert anything. Everything you write must come from what you read here.
- If the workspace does not contain the answer, finish and say so. Do not write it from \
general knowledge.
- Cite meetings and notes by their titles in the note you write.
- You have at most {MAX_STEPS} steps. Write the note before you run out."
    )
}

/// Pull an action out of whatever the model actually said.
///
/// Models wrap JSON in code fences, prefix it with "Sure!", or emit a paragraph and then the
/// JSON. All three are recovered here. What is not recovered — no JSON object at all, or an
/// unknown action name — comes back as [`Action::Unparseable`] and is handed to the model as
/// an observation, which in practice corrects it on the next turn.
pub fn parse_action(raw: &str) -> Action {
    let Some(value) = object_with(raw, "action") else {
        return Action::Unparseable(raw.trim().to_string());
    };

    let field = |name: &str| {
        value
            .get(name)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    match value.get("action").and_then(|a| a.as_str()) {
        Some("search") => match field("query") {
            Some(query) => Action::Search { query },
            None => Action::Unparseable("search needs a non-empty \"query\"".into()),
        },
        Some("read") => match (field("kind"), field("id")) {
            (Some(kind), Some(id)) => Action::Read { kind, id },
            _ => Action::Unparseable("read needs both \"kind\" and \"id\"".into()),
        },
        Some("recent_meetings") => Action::RecentMeetings,
        Some("write_note") => match (field("title"), field("body")) {
            (Some(title), Some(body)) => Action::WriteNote { title, body },
            _ => Action::Unparseable("write_note needs both \"title\" and \"body\"".into()),
        },
        Some("finish") => Action::Finish {
            message: field("message").unwrap_or_else(|| "Finished.".into()),
        },
        Some(other) => Action::Unparseable(format!("\"{other}\" is not one of the actions")),
        None => Action::Unparseable(raw.trim().to_string()),
    }
}

/// The first JSON object in `raw` that carries `key`.
///
/// Two things make this less trivial than `find('{')` … `rfind('}')`.
///
/// The first is nesting and strings: a `}` inside a quoted value does not close the object, so
/// the span has to be found by counting depth while tracking string state and escapes.
///
/// The second is that the first brace in the output is often not the object. Models write
/// things like `I'll use the {search} tool` and *then* emit the real JSON. So every brace is a
/// candidate, and a candidate is only accepted if it parses and actually contains the key
/// being looked for. `{search}` fails both tests and the scan moves on.
fn object_with(raw: &str, key: &str) -> Option<serde_json::Value> {
    for (start, _) in raw.match_indices('{') {
        let Some(span) = balanced_span(raw, start) else {
            // Nothing from here closes, so nothing later can either.
            break;
        };
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(span) {
            if value.get(key).is_some() {
                return Some(value);
            }
        }
    }
    None
}

/// The balanced `{...}` beginning at `start`, or `None` if it never closes.
fn balanced_span(raw: &str, start: usize) -> Option<&str> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, byte) in raw.as_bytes().iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }

        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..=offset]);
                }
            }
            _ => {}
        }
    }

    None
}

// ---------------------------------------------------------------- the loop

/// Start a run in the background and return it immediately.
///
/// Detached rather than awaited: a run is a dozen model calls, which on a local backend is
/// minutes. Holding an HTTP connection open for that is the same mistake the download
/// endpoints exist to avoid.
/// Why an unattended run did not produce anything.
#[derive(Debug)]
pub struct RunFailure {
    pub message: String,
    /// Distinguished from an ordinary failure because it means the schedule or the timeout is wrong,
    /// not the task — and that is a different thing for a user to fix.
    pub timed_out: bool,
}

/// Drive a run to completion, with a ceiling on how long it may take.
///
/// [`start`] returns as soon as the run is registered, which is right for a person watching a
/// progress panel. A scheduled job has nobody watching and needs to know the outcome in order to
/// record it, so this waits.
///
/// The timeout matters more here than anywhere else: a run that never finishes would leave a job
/// permanently "already running" and silently stop its schedule forever.
pub async fn run_to_completion(
    state: &Arc<crate::state::AppState>,
    task: &str,
    timeout_secs: i64,
) -> std::result::Result<Run, RunFailure> {
    let run = Run::new(task.to_string());
    let id = run.id;
    state.agents().insert(run).await;

    let ceiling = std::time::Duration::from_secs(timeout_secs.max(1) as u64);
    let driven = tokio::time::timeout(ceiling, drive(state, id, task)).await;

    match driven {
        Ok(Ok(())) => state.agents().get(id).await.ok_or_else(|| RunFailure {
            message: "the run finished but its record was gone".into(),
            timed_out: false,
        }),
        Ok(Err(message)) => {
            state
                .agents()
                .update(id, |run| {
                    run.status = RunStatus::Failed;
                    run.error = Some(message.clone());
                    run.finished_at = Some(Utc::now());
                })
                .await;
            Err(RunFailure {
                message,
                timed_out: false,
            })
        }
        Err(_) => {
            let message = format!("the run did not finish within {timeout_secs}s");
            state
                .agents()
                .update(id, |run| {
                    run.status = RunStatus::Failed;
                    run.error = Some(message.clone());
                    run.finished_at = Some(Utc::now());
                })
                .await;
            Err(RunFailure {
                message,
                timed_out: true,
            })
        }
    }
}

pub async fn start(state: Arc<crate::state::AppState>, task: String) -> Run {
    let run = Run::new(task.clone());
    let id = run.id;
    state.agents().insert(run.clone()).await;

    tokio::spawn(async move {
        match drive(&state, id, &task).await {
            Ok(()) => {}
            Err(error) => {
                state
                    .agents()
                    .update(id, |run| {
                        run.status = RunStatus::Failed;
                        run.error = Some(error);
                        run.finished_at = Some(Utc::now());
                    })
                    .await;
            }
        }
    });

    run
}

async fn drive(
    state: &Arc<crate::state::AppState>,
    id: Id,
    task: &str,
) -> std::result::Result<(), String> {
    let mut transcript: Vec<ChatMessage> = vec![ChatMessage {
        role: Role::User,
        content: "Begin. Reply with one JSON action.".into(),
    }];

    for step in 1..=MAX_STEPS {
        let last = step == MAX_STEPS;

        let mut messages = transcript.clone();
        if last {
            // Spending the final turn on another search leaves the user with a trace and no
            // write-up, which reads as a crash. Say so explicitly on the last turn.
            messages.push(ChatMessage {
                role: Role::User,
                content: "This is your last step. Use write_note if you have something to \
                          write, otherwise finish and say what you found."
                    .into(),
            });
        }

        // Standing facts about the person, global only: an agent run has no project, and one that
        // researched across the workspace would be scoped to whichever project it happened to read
        // first. Fetched once per turn rather than once per run so a memory added mid-run is not
        // ignored, which costs one cheap read against a call that is already talking to a model.
        let memories = crate::memory::for_prompt(state, None, task).await;

        let mut context = vec![system_prompt(task)];
        if !memories.is_empty() {
            context.push(memories);
        }

        let request = ChatRequest::new(messages).with_context(context);
        let response = state
            .ai()
            .chat(&request)
            .await
            .map_err(|e| format!("the model could not be reached: {e}"))?;

        let action = parse_action(&response.text);
        let reason = reason_of(&response.text);

        let (name, observation, finished) = execute(state, id, &action).await;

        state
            .agents()
            .update(id, |run| {
                run.steps.push(Step {
                    n: step,
                    action: name.to_string(),
                    reason: reason.clone(),
                    observation: retrieval_trim(&observation),
                });
            })
            .await;

        if let Some(message) = finished {
            state
                .agents()
                .update(id, |run| {
                    run.status = RunStatus::Done;
                    run.result = Some(message);
                    run.finished_at = Some(Utc::now());
                })
                .await;
            return Ok(());
        }

        transcript.push(ChatMessage {
            role: Role::Assistant,
            content: response.text,
        });
        transcript.push(ChatMessage {
            role: Role::User,
            content: format!("Result:\n{observation}\n\nNext action?"),
        });
    }

    // Every step used without a `finish`. The run still succeeded if a note was written.
    state
        .agents()
        .update(id, |run| {
            run.status = RunStatus::Done;
            run.result = Some(match run.note_id {
                Some(_) => "Ran out of steps after writing the note.".into(),
                None => "Ran out of steps without reaching a conclusion.".into(),
            });
            run.finished_at = Some(Utc::now());
        })
        .await;
    Ok(())
}

/// The model's stated reason, if the object carried one.
fn reason_of(raw: &str) -> Option<String> {
    object_with(raw, "reason")?
        .get("reason")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Trim an observation for the stored trace. The model saw the full text; the UI does not
/// need four thousand characters per step.
fn retrieval_trim(text: &str) -> String {
    const DISPLAY: usize = 600;
    if text.len() <= DISPLAY {
        return text.to_string();
    }
    let mut end = DISPLAY;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Run one action. Returns its name, what to tell the model, and a closing message if the run
/// is over.
async fn execute(
    state: &Arc<crate::state::AppState>,
    run_id: Id,
    action: &Action,
) -> (&'static str, String, Option<String>) {
    match action {
        Action::Search { query } => {
            // Hybrid, so the agent finds a meeting that discussed "cost structure" when it
            // searched for "pricing". Falls back to lexical on its own when the workspace has
            // no embeddings — the agent should never have to know which is running.
            let found = match retrieval::gather_hybrid(state, query).await {
                Ok(found) => found,
                Err(e) => return ("search", format!("Search failed: {e}"), None),
            };

            if found.is_empty() {
                let terms = retrieval::terms(query);
                return (
                    "search",
                    if terms.is_empty() {
                        "That query has no searchable words in it. Use concrete terms.".into()
                    } else {
                        format!(
                            "No matches for {terms:?}. Try wording closer to what would \
                             actually have been said."
                        )
                    },
                    None,
                );
            }

            // Ids and titles, with an excerpt short enough that a dozen results still leave
            // room for the agent to think. Reading in full is a separate action.
            let lines: Vec<String> = found
                .iter()
                .map(|passage| {
                    format!(
                        "- {} {} \"{}\": {}",
                        passage.kind,
                        passage.id,
                        passage.title,
                        excerpt(&passage.text)
                    )
                })
                .collect();

            ("search", lines.join("\n"), None)
        }

        Action::Read { kind, id } => {
            let Ok(parsed) = id.parse::<Id>() else {
                return (
                    "read",
                    format!("\"{id}\" is not an id. Use an id from a search result."),
                    None,
                );
            };

            let db = state.db().await;
            let text = match kind.as_str() {
                "meeting" => MeetingRepository::new(&db)
                    .get(parsed)
                    .and_then(|m| {
                        Ok(format!(
                            "Meeting \"{}\" ({}):\n{}",
                            m.title,
                            m.started_at.format("%Y-%m-%d"),
                            MeetingRepository::new(&db).transcript_text(parsed)?
                        ))
                    })
                    .map_err(|e| e.to_string()),
                "note" => NoteRepository::new(&db)
                    .get(parsed)
                    .map_err(|e| e.to_string())
                    .and_then(|n| match n.deleted_at {
                        Some(_) => Err("that note is in the trash".to_string()),
                        None => Ok(format!("Note \"{}\":\n{}", n.title, n.body)),
                    }),
                "ticket" => TicketRepository::new(&db)
                    .get(parsed)
                    .map(|t| {
                        format!(
                            "Ticket \"{}\" [{}]:\n{}",
                            t.title,
                            t.status.as_str(),
                            t.description.unwrap_or_default()
                        )
                    })
                    .map_err(|e| e.to_string()),
                other => Err(format!(
                    "\"{other}\" is not readable; use meeting, note or ticket"
                )),
            };

            match text {
                Ok(text) => ("read", truncate_for_model(&text), None),
                Err(e) => ("read", format!("Could not read that: {e}"), None),
            }
        }

        Action::RecentMeetings => {
            let db = state.db().await;
            match MeetingRepository::new(&db).list_recent(15) {
                Ok(meetings) if meetings.is_empty() => (
                    "recent_meetings",
                    "There are no meetings in this workspace yet.".into(),
                    None,
                ),
                Ok(meetings) => (
                    "recent_meetings",
                    meetings
                        .into_iter()
                        .map(|m| {
                            format!(
                                "- meeting {} \"{}\" ({})",
                                m.id,
                                m.title,
                                m.started_at.format("%Y-%m-%d")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    None,
                ),
                Err(e) => (
                    "recent_meetings",
                    format!("Could not list meetings: {e}"),
                    None,
                ),
            }
        }

        Action::WriteNote { title, body } => {
            let created = {
                let db = state.db().await;
                NoteRepository::new(&db).create(NewNote {
                    project_id: None,
                    title: title.clone(),
                    body: body.clone(),
                })
            };

            match created {
                Ok(note) => {
                    let title = note.title.clone();
                    state
                        .agents()
                        .update(run_id, |run| {
                            run.note_id = Some(note.id);
                            run.note_title = Some(title.clone());
                        })
                        .await;
                    (
                        "write_note",
                        format!("Saved as note {} \"{}\". Now finish.", note.id, note.title),
                        None,
                    )
                }
                Err(e) => ("write_note", format!("Could not save the note: {e}"), None),
            }
        }

        Action::Finish { message } => ("finish", message.clone(), Some(message.clone())),

        Action::Unparseable(what) => (
            "think",
            format!(
                "That was not a valid action ({what}). Reply with exactly one JSON object, \
                 for example {{\"reason\": \"...\", \"action\": \"search\", \"query\": \"...\"}}."
            ),
            None,
        ),
    }
}

/// A one-line excerpt of a passage, for a search listing.
fn excerpt(text: &str) -> String {
    const WIDTH: usize = 220;
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= WIDTH {
        return flat;
    }
    let cut: String = flat.chars().take(WIDTH).collect();
    format!("{cut}…")
}

fn truncate_for_model(text: &str) -> String {
    if text.len() <= READ_CHARS {
        return text.to_string();
    }
    let mut end = READ_CHARS;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…\n[truncated]", &text[..end])
}

// ---------------------------------------------------------------- http

pub(crate) fn router() -> axum::Router<Arc<crate::state::AppState>> {
    use axum::routing::get;
    axum::Router::new()
        .route("/v1/agent/runs", get(list_runs).post(start_run))
        .route("/v1/agent/runs/:id", get(get_run))
}

#[derive(Debug, serde::Deserialize)]
struct StartRun {
    task: String,
}

/// The longest task a run may be given.
///
/// Not a security boundary — the engine is loopback-only and single-user. It stops a pasted
/// document from becoming a system prompt that crowds out the instructions themselves, which
/// produces a run that ignores its own protocol and looks like a bug in the agent.
const MAX_TASK_CHARS: usize = 2_000;

async fn start_run(
    axum::extract::State(state): axum::extract::State<Arc<crate::state::AppState>>,
    axum::Json(body): axum::Json<StartRun>,
) -> crate::error::ApiResult<axum::Json<Run>> {
    let task = body.task.trim().to_string();
    if task.is_empty() {
        return Err(crate::error::ApiError::BadRequest(
            "give the agent something to do".into(),
        ));
    }
    if task.chars().count() > MAX_TASK_CHARS {
        return Err(crate::error::ApiError::BadRequest(format!(
            "that task is too long; keep it under {MAX_TASK_CHARS} characters"
        )));
    }

    Ok(axum::Json(start(state, task).await))
}

async fn get_run(
    axum::extract::State(state): axum::extract::State<Arc<crate::state::AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> crate::error::ApiResult<axum::Json<Run>> {
    let id: Id = id
        .parse()
        .map_err(|_| crate::error::ApiError::BadRequest(format!("'{id}' is not a valid id")))?;

    state
        .agents()
        .get(id)
        .await
        .map(axum::Json)
        // A run that has been evicted is genuinely gone, and saying so is better than an
        // empty run that looks like it never did anything.
        .ok_or_else(|| crate::error::ApiError::NotFound(format!("no agent run {id}")))
}

async fn list_runs(
    axum::extract::State(state): axum::extract::State<Arc<crate::state::AppState>>,
) -> axum::Json<Vec<Run>> {
    axum::Json(state.agents().list().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_object_parses() {
        assert_eq!(
            parse_action(r#"{"action": "search", "query": "pricing"}"#),
            Action::Search {
                query: "pricing".into()
            }
        );
    }

    /// Every local model this app supports does at least one of these.
    #[test]
    fn prose_and_code_fences_around_the_object_are_tolerated() {
        let fenced = "Sure! Here is my next step:\n```json\n{\"action\": \"recent_meetings\"}\n```\nHope that helps.";
        assert_eq!(parse_action(fenced), Action::RecentMeetings);
    }

    /// The reason `find('{')`/`rfind('}')` is not good enough: a brace in the prose would
    /// make the span unparseable, and the run would stall on a turn that was actually fine.
    #[test]
    fn a_brace_in_the_prose_does_not_break_extraction() {
        let raw =
            "I'll use the {search} tool now.\n{\"action\": \"search\", \"query\": \"budget\"}";
        assert_eq!(
            parse_action(raw),
            Action::Search {
                query: "budget".into()
            }
        );
    }

    /// Braces inside a JSON string must not close the object early.
    #[test]
    fn braces_inside_strings_are_not_counted() {
        let raw = r#"{"action": "write_note", "title": "The {curly} report", "body": "a } b"}"#;
        assert_eq!(
            parse_action(raw),
            Action::WriteNote {
                title: "The {curly} report".into(),
                body: "a } b".into()
            }
        );
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        let raw = r#"{"action": "finish", "message": "she said \"done\""}"#;
        assert_eq!(
            parse_action(raw),
            Action::Finish {
                message: "she said \"done\"".into()
            }
        );
    }

    #[test]
    fn missing_arguments_are_reported_rather_than_silently_defaulted() {
        assert!(matches!(
            parse_action(r#"{"action": "search"}"#),
            Action::Unparseable(_)
        ));
        assert!(matches!(
            parse_action(r#"{"action": "read", "kind": "meeting"}"#),
            Action::Unparseable(_)
        ));
        assert!(matches!(
            parse_action(r#"{"action": "write_note", "title": "x"}"#),
            Action::Unparseable(_)
        ));
    }

    #[test]
    fn an_empty_string_argument_counts_as_missing() {
        assert!(matches!(
            parse_action(r#"{"action": "search", "query": "   "}"#),
            Action::Unparseable(_)
        ));
    }

    #[test]
    fn an_unknown_action_names_itself_in_the_correction() {
        match parse_action(r#"{"action": "delete_everything", "id": "x"}"#) {
            Action::Unparseable(message) => assert!(
                message.contains("delete_everything"),
                "the model needs to know which name was wrong: {message}"
            ),
            other => panic!("expected a correction, got {other:?}"),
        }
    }

    #[test]
    fn pure_prose_is_unparseable_rather_than_fatal() {
        assert!(matches!(
            parse_action("I think we should look at the pricing meeting."),
            Action::Unparseable(_)
        ));
    }

    #[test]
    fn finish_without_a_message_still_finishes() {
        assert_eq!(
            parse_action(r#"{"action": "finish"}"#),
            Action::Finish {
                message: "Finished.".into()
            }
        );
    }

    #[test]
    fn the_reason_is_extracted_for_the_trace() {
        assert_eq!(
            reason_of(
                r#"{"reason": "need the pricing meeting", "action": "search", "query": "x"}"#
            ),
            Some("need the pricing meeting".into())
        );
        assert_eq!(reason_of(r#"{"action": "search", "query": "x"}"#), None);
        assert_eq!(reason_of("no json here"), None);
    }

    #[test]
    fn an_unterminated_object_is_not_mistaken_for_one() {
        assert_eq!(balanced_span(r#"{"action": "search""#, 0), None);
        assert!(matches!(
            parse_action(r#"{"action": "search""#),
            Action::Unparseable(_)
        ));
    }

    /// A candidate that parses but is the wrong object must not be accepted. Models emit
    /// scratch JSON before the real action often enough for this to matter.
    #[test]
    fn a_parseable_object_without_an_action_is_skipped() {
        let raw = r#"{"note": "thinking out loud"} then: {"action": "recent_meetings"}"#;
        assert_eq!(parse_action(raw), Action::RecentMeetings);
    }

    #[test]
    fn display_trimming_lands_on_a_character_boundary() {
        let text = "é".repeat(1000);
        let trimmed = retrieval_trim(&text);
        assert!(trimmed.ends_with('…'));
        assert!(trimmed.len() <= 600 + '…'.len_utf8());
    }

    #[test]
    fn the_prompt_states_the_step_budget_it_actually_enforces() {
        // These drifting apart would have the model pacing itself against a wrong number.
        assert!(system_prompt("t").contains(&MAX_STEPS.to_string()));
    }

    // ------------------------------------------------------------ the loop, end to end

    /// A backend that replies with a fixed script, one line per turn.
    ///
    /// The mock backend answers everything with the same text, which would put the agent in a
    /// loop repeating one action. Driving the loop needs replies that *change*, and driving it
    /// through a specific path — search, read, write, finish — needs them chosen in advance.
    /// Shared with the test, so replies can be queued *after* the state exists — which is
    /// necessary whenever a scripted action has to name an id the seed data only just created.
    type Script = Arc<std::sync::Mutex<std::collections::VecDeque<String>>>;

    #[derive(Debug)]
    struct Scripted {
        replies: Script,
        /// The prompts the agent sent, so a test can assert on what the model was told.
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl AiBackend for Scripted {
        fn model_id(&self) -> &str {
            "scripted"
        }
        fn is_local(&self) -> bool {
            true
        }
        async fn summarize(
            &self,
            _: &notewise_ai_router::TranscriptInput,
        ) -> notewise_ai_router::Result<notewise_ai_router::SummaryOutput> {
            unreachable!("the agent only chats")
        }
        async fn extract_decisions(
            &self,
            _: &notewise_ai_router::TranscriptInput,
        ) -> notewise_ai_router::Result<Vec<notewise_ai_router::ExtractedDecision>> {
            unreachable!("the agent only chats")
        }
        async fn extract_action_items(
            &self,
            _: &notewise_ai_router::TranscriptInput,
        ) -> notewise_ai_router::Result<Vec<notewise_ai_router::ExtractedActionItem>> {
            unreachable!("the agent only chats")
        }
        async fn chat(
            &self,
            request: &ChatRequest,
        ) -> notewise_ai_router::Result<notewise_ai_router::ChatResponse> {
            self.seen.lock().unwrap().push(
                request
                    .messages
                    .last()
                    .map(|m| m.content.clone())
                    .unwrap_or_default(),
            );

            let text = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                // Running past the script means the test's expectations and the loop have
                // diverged; finishing keeps it from spinning to MAX_STEPS before failing.
                .unwrap_or_else(|| r#"{"action": "finish", "message": "script exhausted"}"#.into());

            Ok(notewise_ai_router::ChatResponse {
                text,
                model: "scripted".into(),
            })
        }
    }

    /// A state whose backend replies from a queue the caller keeps a handle to.
    fn scripted_state() -> (
        Arc<crate::state::AppState>,
        Script,
        Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let replies: Script = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let state = Arc::new(crate::state::AppState::new(
            notewise_storage::Database::open_in_memory().expect("in-memory db"),
            notewise_ai_router::Router::with_backend(Box::new(Scripted {
                replies: replies.clone(),
                seen: seen.clone(),
            })),
        ));
        (state, replies, seen)
    }

    fn queue(script: &Script, replies: &[String]) {
        let mut queued = script.lock().unwrap();
        for reply in replies {
            queued.push_back(reply.clone());
        }
    }

    /// The common case: a fixed script known before any seeding.
    fn state_with(replies: &[&str]) -> Arc<crate::state::AppState> {
        let (state, script, _) = scripted_state();
        queue(
            &script,
            &replies.iter().map(|r| (*r).to_string()).collect::<Vec<_>>(),
        );
        state
    }

    /// Poll until the run leaves `Running`, with a ceiling so a hang fails rather than hangs.
    async fn settle(state: &Arc<crate::state::AppState>, id: Id) -> Run {
        for _ in 0..400 {
            if let Some(run) = state.agents().get(id).await {
                if run.status != RunStatus::Running {
                    return run;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("the run never finished");
    }

    async fn seed_meeting(state: &Arc<crate::state::AppState>, title: &str, line: &str) -> Id {
        use chrono::TimeZone;
        let db = state.db().await;
        let repo = MeetingRepository::new(&db);
        let meeting = repo
            .create(notewise_storage::NewMeeting {
                project_id: None,
                title: title.into(),
                source: notewise_storage::MeetingSource::Import,
                started_at: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            })
            .expect("meeting");
        repo.add_segment(notewise_storage::NewTranscriptSegment {
            meeting_id: meeting.id,
            speaker: Some("Alex".into()),
            text: line.into(),
            start_ms: 0,
            end_ms: 1000,
            confidence: None,
        })
        .expect("segment");
        meeting.id
    }

    #[tokio::test]
    async fn a_run_searches_reads_writes_a_note_and_finishes() {
        let (state, script, _) = scripted_state();
        let meeting = seed_meeting(&state, "Pricing review", "we settled on three tiers").await;

        queue(
            &script,
            &[
                r#"{"reason": "find it", "action": "search", "query": "pricing"}"#.to_string(),
                format!(r#"{{"action": "read", "kind": "meeting", "id": "{meeting}"}}"#),
                r#"{"action": "write_note", "title": "Pricing", "body": "Three tiers."}"#
                    .to_string(),
                r#"{"action": "finish", "message": "Wrote it up."}"#.to_string(),
            ],
        );

        let started = start(state.clone(), "write up the pricing decision".into()).await;
        let run = settle(&state, started.id).await;

        assert_eq!(run.status, RunStatus::Done, "{run:?}");
        assert_eq!(run.result.as_deref(), Some("Wrote it up."));
        assert_eq!(
            run.steps
                .iter()
                .map(|s| s.action.as_str())
                .collect::<Vec<_>>(),
            vec!["search", "read", "write_note", "finish"]
        );
        assert_eq!(run.steps[0].reason.as_deref(), Some("find it"));

        let note_id = run.note_id.expect("a note should have been written");
        let db = state.db().await;
        assert_eq!(
            NoteRepository::new(&db).get(note_id).unwrap().title,
            "Pricing"
        );
    }

    #[tokio::test]
    async fn an_unparseable_turn_is_corrected_rather_than_fatal() {
        let state = state_with(&[
            "I think I should look at the meetings.",
            r#"{"action": "finish", "message": "ok"}"#,
        ]);

        let started = start(state.clone(), "do something".into()).await;
        let run = settle(&state, started.id).await;

        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(run.steps[0].action, "think");
        assert!(
            run.steps[0].observation.contains("not a valid action"),
            "the model must be told what went wrong: {}",
            run.steps[0].observation
        );
    }

    /// The loop must stop on its own. Without the ceiling a model that never emits `finish`
    /// runs until someone kills the process — on a metered backend, expensively.
    #[tokio::test]
    async fn a_run_that_never_finishes_stops_at_the_step_ceiling() {
        let state = state_with(&["not json, ever"; MAX_STEPS + 10]);

        let started = start(state.clone(), "loop forever".into()).await;
        let run = settle(&state, started.id).await;

        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(run.steps.len(), MAX_STEPS);
        assert!(
            run.result.as_deref().unwrap().contains("Ran out of steps"),
            "{:?}",
            run.result
        );
        assert_eq!(run.note_id, None);
    }

    #[tokio::test]
    async fn a_backend_failure_fails_the_run_with_a_reason() {
        let state = Arc::new(crate::state::AppState::new(
            notewise_storage::Database::open_in_memory().expect("db"),
            notewise_ai_router::Router::with_backend(Box::new(
                notewise_ai_router::MockBackend::failing("ollama is not running"),
            )),
        ));

        let started = start(state.clone(), "anything".into()).await;
        let run = settle(&state, started.id).await;

        assert_eq!(run.status, RunStatus::Failed);
        assert!(
            run.error
                .as_deref()
                .unwrap()
                .contains("ollama is not running"),
            "{:?}",
            run.error
        );
    }

    /// Reading something that is not there must come back as a correction the model can act
    /// on, not as a dead run.
    #[tokio::test]
    async fn reading_a_missing_item_is_reported_to_the_model() {
        let state = state_with(&[
            r#"{"action": "read", "kind": "meeting", "id": "not-an-id"}"#,
            r#"{"action": "finish", "message": "gave up"}"#,
        ]);

        let started = start(state.clone(), "read something".into()).await;
        let run = settle(&state, started.id).await;

        assert_eq!(run.status, RunStatus::Done);
        assert!(
            run.steps[0].observation.contains("is not an id"),
            "{:?}",
            run.steps[0]
        );
    }

    #[tokio::test]
    async fn a_search_with_no_matches_says_so_rather_than_returning_nothing() {
        let state = state_with(&[
            r#"{"action": "search", "query": "kubernetes"}"#,
            r#"{"action": "finish", "message": "not here"}"#,
        ]);

        let started = start(state.clone(), "find kubernetes".into()).await;
        let run = settle(&state, started.id).await;

        assert!(
            run.steps[0].observation.contains("No matches"),
            "{:?}",
            run.steps[0]
        );
    }

    /// The last turn has to ask for a conclusion, or a run ends with a trace and no output.
    #[tokio::test]
    async fn the_final_step_is_told_it_is_the_final_step() {
        let (state, script, seen) = scripted_state();
        queue(&script, &vec!["nope".to_string(); MAX_STEPS]);

        let started = start(state.clone(), "x".into()).await;
        settle(&state, started.id).await;

        let prompts = seen.lock().unwrap();
        assert_eq!(prompts.len(), MAX_STEPS);
        assert!(
            prompts.last().unwrap().contains("This is your last step"),
            "the final prompt must ask for a conclusion: {:?}",
            prompts.last()
        );
        assert!(
            !prompts[MAX_STEPS - 2].contains("This is your last step"),
            "only the final step gets that instruction"
        );
    }

    #[tokio::test]
    async fn finished_runs_are_evicted_but_running_ones_are_not() {
        let registry = AgentRegistry::new();

        let mut live = Run::new("still going".into());
        live.started_at = Utc::now() - chrono::Duration::hours(1);
        let live_id = live.id;
        registry.insert(live).await;

        for n in 0..MAX_RETAINED + 5 {
            let mut run = Run::new(format!("done {n}"));
            run.status = RunStatus::Done;
            registry.insert(run).await;
        }

        assert!(
            registry.get(live_id).await.is_some(),
            "a run still in progress must never be evicted"
        );
        assert!(registry.list().await.len() <= MAX_RETAINED + 1);
    }
}
