//! The live half of the guide: what the workspace holds and how it is set up, right now.
//!
//! A guide section can say what a setting does; only the engine can say what it is set to. Each
//! [`Live`] value here is read from the same place the matching Settings page or list reads it,
//! so Ask and the screen cannot disagree about which model is running or how much is in the
//! trash.
//!
//! # Never cached
//!
//! A cached count is wrong the moment something is recorded or deleted, and every value here is
//! a handful of local queries. They are worked out per question, and only for the sections that
//! question found ([`crate::guide::passages`]).
//!
//! # Failures cost the value, never the answer
//!
//! A value that cannot be read is left out and logged. The section still says what the setting
//! does, and the question is still answered.

use std::sync::Arc;

use axum::extract::State;
use chrono::{DateTime, Local, Utc};
use notewise_storage::{
    ActionItem, Database, MeetingRepository, NoteRepository, SettingsRepository, SummaryRepository,
    WorkStatus,
};
use notewise_transcription::ModelRegistry;

use crate::guide::Live;
use crate::routes::CaptureKindArg;
use crate::state::AppState;

/// How many recordings "Recent recordings" lists.
const RECENT: u32 = 15;

/// How many tasks, and how many trashed items, a list shows before saying how many more there are.
const LISTED: usize = 20;

/// The value, in a form a model can quote, or `None` when it cannot be read.
pub async fn render(state: &Arc<AppState>, live: Live) -> Option<String> {
    let rendered = match live {
        Live::Numbers => from_db(state, numbers).await,
        Live::Recent => from_db(state, recent).await,
        Live::Tasks => from_db(state, tasks).await,
        Live::Trash => from_db(state, trash).await,
        Live::Models => Some(models(state).await),
        Live::Appearance => from_db(state, appearance).await,
        Live::Transcription => Some(transcription(state)),
        Live::Recording => Some(recording()),
        Live::Retention => retention(state).await,
        Live::Detection => detection(state).await,
        Live::Search => Some(search(state).await),
        Live::Speakers => speakers(state).await,
        Live::About => from_db(state, |db| about(state, db)).await,
    };
    if rendered.is_none() {
        tracing::debug!(live = live.as_str(), "live value unavailable; left out");
    }
    rendered
}

async fn from_db(
    state: &Arc<AppState>,
    read: impl FnOnce(&Database) -> notewise_storage::Result<String>,
) -> Option<String> {
    let db = state.db().await;
    read(&db)
        .map_err(|error| tracing::debug!(%error, "live value could not be read"))
        .ok()
}

/// A date as people say it, with its weekday, so "on Monday" and "this week" can be answered.
fn day(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local).format("%A %-d %B %Y").to_string()
}

/// What today is, for a list that "yesterday" or "due this week" is asked of.
///
/// Measured on `llama3.1:8b`: asked "what did I record yesterday?" on the 31st, with recordings
/// listed on the 29th and 28th and no date for today, it answered with the 29th.
fn today() -> String {
    format!("Today is {}.", day(Utc::now()))
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

// ---------------------------------------------------------------- the workspace

fn numbers(db: &Database) -> notewise_storage::Result<String> {
    let meetings = MeetingRepository::new(db);
    let notes = NoteRepository::new(db);
    let recordings = meetings.count(false)?;

    let mut lines = vec![format!(
        "Recordings: {recordings}{}.",
        meetings
            .first_started_at()?
            .map(|first| format!(", the first on {}", day(first)))
            .unwrap_or_default()
    )];
    lines.push(format!("Notes: {}.", notes.count(false)?));

    let by_status = SummaryRepository::new(db).count_action_items_by_status()?;
    let of = |status: WorkStatus| {
        by_status
            .iter()
            .find(|(s, _)| *s == status)
            .map_or(0, |(_, count)| *count)
    };
    let open = of(WorkStatus::Todo) + of(WorkStatus::InProgress);
    lines.push(format!(
        "Tasks: {open} open ({} to do, {} in progress), {} done, {} cancelled.",
        of(WorkStatus::Todo),
        of(WorkStatus::InProgress),
        of(WorkStatus::Done),
        of(WorkStatus::Cancelled)
    ));
    lines.push(format!(
        "In the trash: {} and {}.",
        plural(meetings.count(true)?, "recording", "recordings"),
        plural(notes.count(true)?, "note", "notes")
    ));
    Ok(format!("Right now:\n{}", lines.join("\n")))
}

fn recent(db: &Database) -> notewise_storage::Result<String> {
    let meetings = MeetingRepository::new(db).list_recent(RECENT)?;
    if meetings.is_empty() {
        return Ok("Right now there are no recordings.".into());
    }
    let lines: Vec<String> = meetings
        .iter()
        .map(|meeting| {
            let length = match meeting.ended_at {
                Some(ended) => {
                    let minutes = (ended - meeting.started_at).num_minutes().max(0);
                    if minutes < 1 {
                        "under a minute".to_string()
                    } else {
                        plural(minutes as u64, "minute", "minutes")
                    }
                }
                None => "still recording".to_string(),
            };
            format!(
                "- \"{}\", {}, {length}",
                meeting.title,
                day(meeting.started_at)
            )
        })
        .collect();
    Ok(format!(
        "{} The {} most recent, newest first:\n{}",
        today(),
        lines.len(),
        lines.join("\n")
    ))
}

fn tasks(db: &Database) -> notewise_storage::Result<String> {
    let summaries = SummaryRepository::new(db);
    let meetings = MeetingRepository::new(db);
    let open: u64 = summaries
        .count_action_items_by_status()?
        .iter()
        .filter(|(status, _)| matches!(status, WorkStatus::Todo | WorkStatus::InProgress))
        .map(|(_, count)| count)
        .sum();
    if open == 0 {
        return Ok("Right now there are no open tasks.".into());
    }

    let describe = |item: &ActionItem| {
        let mut line = format!("- {}", item.text);
        if let Some(owner) = &item.owner {
            line.push_str(&format!(", owner {owner}"));
        }
        if let Some(due) = item.due_at {
            line.push_str(&format!(", due {}", day(due)));
        }
        if item.status == WorkStatus::InProgress {
            line.push_str(", in progress");
        }
        if let Ok(meeting) = meetings.get(item.meeting_id) {
            line.push_str(&format!(", from \"{}\"", meeting.title));
        }
        line
    };
    let listed: Vec<String> = summaries
        .open_action_items(LISTED as u32)?
        .iter()
        .map(describe)
        .collect();
    let more = open.saturating_sub(listed.len() as u64);

    Ok(format!(
        "{} There {} open, soonest due first:\n{}{}",
        today(),
        if open == 1 {
            "is 1 task".to_string()
        } else {
            format!("are {open} tasks")
        },
        listed.join("\n"),
        if more > 0 {
            format!("\nand {more} more.")
        } else {
            String::new()
        }
    ))
}

fn trash(db: &Database) -> notewise_storage::Result<String> {
    let meetings = MeetingRepository::new(db).list_trashed()?;
    let notes = NoteRepository::new(db).list_trashed()?;
    if meetings.is_empty() && notes.is_empty() {
        return Ok("Right now the trash is empty.".into());
    }

    let mut items: Vec<(Option<DateTime<Utc>>, String)> = meetings
        .iter()
        .map(|m| (m.deleted_at, format!("recording \"{}\"", m.title)))
        .chain(
            notes
                .iter()
                .map(|n| (n.deleted_at, format!("note \"{}\"", n.title))),
        )
        .collect();
    items.sort_by_key(|(deleted, _)| std::cmp::Reverse(*deleted));

    let total = items.len();
    let lines: Vec<String> = items
        .iter()
        .take(LISTED)
        .map(|(deleted, what)| match deleted {
            Some(at) => format!("- {what}, deleted {}", day(*at)),
            None => format!("- {what}"),
        })
        .collect();

    Ok(format!(
        "Right now the trash holds {} and {}, most recently deleted first:\n{}{}",
        plural(meetings.len() as u64, "recording", "recordings"),
        plural(notes.len() as u64, "note", "notes"),
        lines.join("\n"),
        if total > LISTED {
            format!("\nand {} more.", total - LISTED)
        } else {
            String::new()
        }
    ))
}

// ---------------------------------------------------------------- settings

async fn models(state: &Arc<AppState>) -> String {
    let ai = state.ai();
    let routes = ai.route_names();
    let mut text = format!(
        "Right now: {} through {}, {}.",
        ai.resolved_model_label().await,
        ai.kind().label(),
        if ai.is_local() {
            "on this machine"
        } else {
            "sent to that provider"
        }
    );
    if !routes.is_empty() {
        text.push_str(&format!(" Routing rules in use: {}.", routes.join(", ")));
    }
    text
}

fn appearance(db: &Database) -> notewise_storage::Result<String> {
    // Stored opaquely by the window, so read best-effort: what is not there was never changed.
    let stored: serde_json::Value = SettingsRepository::new(db)
        .get(crate::routes::PREFERENCES_KEY)?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let mode = stored["mode"].as_str().unwrap_or("dark");
    let accent = stored["accent"].as_str().unwrap_or("the default");
    Ok(format!("Right now: {mode} theme, {accent} accent."))
}

fn transcription(state: &Arc<AppState>) -> String {
    let installed: Vec<String> = state
        .model_store()
        .installed()
        .into_iter()
        .map(|model| model.name.to_string())
        .collect();
    let recommended = ModelRegistry::default_model().name;
    if installed.is_empty() {
        format!(
            "Right now no transcription model is downloaded. The recommended one is {recommended}."
        )
    } else {
        format!(
            "Right now downloaded: {}. The recommended one is {recommended}.",
            installed.join(", ")
        )
    }
}

fn recording() -> String {
    // Read without prompting: asking a question must never raise a permission dialog.
    let microphone = crate::routes::permission_readiness(CaptureKindArg::Microphone, false);
    let system = crate::routes::permission_readiness(CaptureKindArg::SystemAudio, false);
    format!(
        "Right now: the microphone is {}, and system audio is {}.",
        permission(&microphone.status),
        permission(&system.status)
    )
}

/// A permission's status in words. Measured on `llama3.1:8b`: given "microphone unavailable", it
/// answered that the passage "does not explicitly state that microphone permission is granted or
/// denied".
fn permission(status: &str) -> &str {
    match status {
        "granted" => "allowed",
        "denied" => "not allowed, and has to be switched on in macOS System Settings",
        "not_requested" => "not asked for yet",
        "unavailable" => "not available in this build",
        other => other,
    }
}

async fn retention(state: &Arc<AppState>) -> Option<String> {
    let body = crate::routing::get_retention(State(state.clone()))
        .await
        .ok()?
        .0;
    let policy = match body.policy.as_str() {
        "off" => "audio is not kept after transcription".to_string(),
        "until_deleted" => {
            "audio is kept until the recording is deleted from the trash".to_string()
        }
        other => match other.strip_prefix("days:") {
            Some(days) => format!("audio is kept for {days} days after a recording ends"),
            None => other.to_string(),
        },
    };
    Some(format!(
        "Right now {policy}. {} kept, using {} MB.",
        plural(
            body.retained as u64,
            "recording has audio",
            "recordings have audio"
        ),
        body.bytes / 1_000_000
    ))
}

async fn detection(state: &Arc<AppState>) -> Option<String> {
    let status = crate::join::detection_status(State(state.clone()))
        .await
        .ok()?
        .0;
    let extension = status
        .sources
        .iter()
        .find(|source| source.source.contains("extension"))
        .and_then(|source| source.last_seen_at.as_deref());
    Some(format!(
        "Right now: a calendar is {}connected, and the browser extension {}.",
        if status.calendar_connected {
            ""
        } else {
            "not "
        },
        match extension.map(DateTime::parse_from_rfc3339) {
            Some(Ok(seen)) => format!(
                "was last heard from on {}",
                seen.with_timezone(&Local).format("%-d %B %Y at %H:%M")
            ),
            Some(Err(_)) => "has been heard from".to_string(),
            None => "has never been heard from".to_string(),
        }
    ))
}

async fn search(state: &Arc<AppState>) -> String {
    let status = crate::indexing::index_status(State(state.clone())).await.0;
    format!(
        "Right now: {} passages are indexed for meaning with {}, which is {}.",
        status.chunks,
        status.model,
        if status.available {
            "available"
        } else {
            "not reachable, so search matches words only"
        }
    )
}

async fn speakers(state: &Arc<AppState>) -> Option<String> {
    let status = crate::diarization::get_status(State(state.clone()))
        .await
        .ok()?
        .0;
    Some(match status.blocked_by {
        None => format!(
            "Right now: speakers are separated by voice, with {}.",
            status.model
        ),
        Some(reason) => format!("Right now speakers are not separated by voice: {reason}"),
    })
}

fn about(state: &Arc<AppState>, db: &Database) -> notewise_storage::Result<String> {
    Ok(format!(
        "Right now: version {}, database schema {}. {} AI {}.",
        env!("CARGO_PKG_VERSION"),
        db.schema_version()?,
        if crate::recording::SUPPORTED && state.db_path().is_some() {
            "This build can record from the microphone."
        } else {
            "This build cannot record; it can import audio files."
        },
        if state.ai().is_local() {
            "runs on this machine"
        } else {
            "is sent to the configured provider"
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use notewise_ai_router::{Router as AiRouter, RouterConfig};
    use notewise_storage::{MeetingSource, NewActionItem, NewMeeting, NewNote, NewSummary};

    fn state() -> Arc<AppState> {
        Arc::new(AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ))
    }

    async fn meeting(state: &Arc<AppState>, title: &str) -> notewise_storage::Meeting {
        let db = state.db().await;
        MeetingRepository::new(&db)
            .create(NewMeeting {
                project_id: None,
                title: title.into(),
                source: MeetingSource::Import,
                started_at: Utc.timestamp_opt(1_790_000_000, 0).single().unwrap(),
            })
            .unwrap()
    }

    #[tokio::test]
    async fn the_numbers_count_what_library_my_tasks_and_trash_show() {
        let state = state();
        meeting(&state, "Kickoff").await;
        let planning = meeting(&state, "Planning").await;
        let gone = meeting(&state, "Old standup").await;
        {
            let db = state.db().await;
            let summaries = SummaryRepository::new(&db);
            let summary = summaries
                .create(NewSummary {
                    meeting_id: planning.id,
                    text: "Plan.".into(),
                    model: "mock".into(),
                    template_id: None,
                })
                .unwrap();
            summaries
                .add_action_item(NewActionItem::from_summary(&summary, "Send the deck"))
                .unwrap();
            MeetingRepository::new(&db).trash(gone.id).unwrap();
            NoteRepository::new(&db)
                .create(NewNote {
                    project_id: None,
                    title: "Ideas".into(),
                    body: "body".into(),
                })
                .unwrap();
        }

        let numbers = render(&state, Live::Numbers).await.expect("numbers");
        assert!(numbers.contains("Recordings: 2,"), "{numbers}");
        assert!(numbers.contains("Notes: 1."), "{numbers}");
        assert!(numbers.contains("Tasks: 1 open"), "{numbers}");
        assert!(
            numbers.contains("In the trash: 1 recording and 0 notes."),
            "{numbers}"
        );

        let tasks = render(&state, Live::Tasks).await.expect("tasks");
        assert!(tasks.contains("Send the deck"), "{tasks}");
        assert!(tasks.contains("from \"Planning\""), "{tasks}");

        let trash = render(&state, Live::Trash).await.expect("trash");
        assert!(trash.contains("recording \"Old standup\""), "{trash}");

        let recent = render(&state, Live::Recent).await.expect("recent");
        assert!(recent.contains("\"Kickoff\""), "{recent}");
        assert!(!recent.contains("Old standup"), "trashed: {recent}");
    }

    #[tokio::test]
    async fn an_empty_workspace_says_so() {
        let state = state();
        assert!(render(&state, Live::Recent)
            .await
            .unwrap()
            .contains("no recordings"));
        assert!(render(&state, Live::Tasks)
            .await
            .unwrap()
            .contains("no open tasks"));
        assert!(render(&state, Live::Trash).await.unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn appearance_reads_what_the_window_stored() {
        let state = state();
        assert!(render(&state, Live::Appearance)
            .await
            .unwrap()
            .contains("dark theme"));
        {
            let db = state.db().await;
            SettingsRepository::new(&db)
                .set(
                    crate::routes::PREFERENCES_KEY,
                    r#"{"mode":"light","accent":"ocean"}"#,
                )
                .unwrap();
        }
        assert_eq!(
            render(&state, Live::Appearance).await.unwrap(),
            "Right now: light theme, ocean accent."
        );
    }

    /// Every value renders on a bare engine. None of them may need a daemon, a grant or a key to
    /// produce something, even if what it produces is "not available".
    #[tokio::test]
    async fn every_value_renders_on_a_bare_engine() {
        let state = state();
        for live in Live::ALL {
            let value = render(&state, live).await;
            assert!(
                value.as_deref().is_some_and(|v| !v.is_empty()),
                "{} rendered {value:?}",
                live.as_str()
            );
        }
    }
}
