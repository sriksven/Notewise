//! Stored entities.
//!
//! Fields here model **ownership** — a transcript segment belongs to exactly one meeting,
//! expressed as a foreign key. **Association** between entities (a note referencing a
//! meeting, an action item linked to a ticket) is modelled as typed edges in the `graph`
//! crate, not as columns here. See docs/architecture/overview.md.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::Id;

/// Top-level container. A single-user local install has exactly one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Id,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Organizing unit. Rolls up its meetings, notes, and tickets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: Id,
    pub workspace_id: Id,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Where a meeting's audio came from. Determines which capture path produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingSource {
    /// Captured from the local machine's microphone.
    Microphone,
    /// Captured from system/loopback audio (the other participants).
    SystemAudio,
    /// Both mic and system audio mixed.
    Combined,
    /// Streamed from the browser extension's tab capture.
    BrowserTab,
    /// Uploaded or imported after the fact.
    Import,
}

impl MeetingSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            MeetingSource::Microphone => "microphone",
            MeetingSource::SystemAudio => "system_audio",
            MeetingSource::Combined => "combined",
            MeetingSource::BrowserTab => "browser_tab",
            MeetingSource::Import => "import",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "microphone" => MeetingSource::Microphone,
            "system_audio" => MeetingSource::SystemAudio,
            "combined" => MeetingSource::Combined,
            "browser_tab" => MeetingSource::BrowserTab,
            "import" => MeetingSource::Import,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meeting {
    pub id: Id,
    pub project_id: Option<Id>,
    pub title: String,
    pub source: MeetingSource,
    pub started_at: DateTime<Utc>,
    /// `None` while a meeting is still recording.
    pub ended_at: Option<DateTime<Utc>>,
    /// The recurring series this instance belongs to, if any.
    pub series_id: Option<Id>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A human. Not a workspace member: most people in a user's meetings will never have an
/// account, and requiring one would make attribution impossible for exactly the people it
/// matters most for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: Id,
    pub display_name: String,
    pub email: Option<String>,
    /// Whether a voiceprint has been enrolled for this person.
    ///
    /// The vector itself is deliberately not on this struct. It is biometric data, it is
    /// large, and nothing outside speaker matching has any business holding it — so it is
    /// fetched explicitly rather than riding along on every read of a person.
    pub has_voice_print: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A recurring meeting. Threading instances is what lets unfinished business carry forward
/// rather than being rediscovered by hand each week.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingSeries {
    pub id: Id,
    pub title: String,
    pub project_id: Option<Id>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Meeting {
    /// Duration in milliseconds, or `None` if still recording.
    pub fn duration_ms(&self) -> Option<i64> {
        self.ended_at
            .map(|end| (end - self.started_at).num_milliseconds())
    }

    pub fn is_recording(&self) -> bool {
        self.ended_at.is_none()
    }
}

/// One timestamped chunk of transcript. Emitted by `transcription`, speaker labels
/// populated later by `diarization`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub id: Id,
    pub meeting_id: Id,
    /// `None` until diarization runs, or if diarization is disabled.
    pub speaker: Option<String>,
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
    /// Engine confidence in `0.0..=1.0`, when the engine reports one.
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub id: Id,
    pub meeting_id: Id,
    pub text: String,
    /// Which model produced this, so a summary can be regenerated or audited later.
    pub model: String,
    pub created_at: DateTime<Utc>,
}

/// A decision reached in a meeting. Kept distinct from action items because decisions are
/// a record of *what was settled*, not work to be done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub id: Id,
    /// The meeting where this was decided. Ownership lives here, not on the summary.
    pub meeting_id: Id,
    /// Which summary first surfaced this, if any — provenance, not ownership. Becomes
    /// `None` when that summary is regenerated; the decision itself survives.
    pub summary_id: Option<Id>,
    pub text: String,
    pub reasoning: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
    Todo,
    InProgress,
    Done,
    Cancelled,
}

impl WorkStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkStatus::Todo => "todo",
            WorkStatus::InProgress => "in_progress",
            WorkStatus::Done => "done",
            WorkStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "todo" => WorkStatus::Todo,
            "in_progress" => WorkStatus::InProgress,
            "done" => WorkStatus::Done,
            "cancelled" => WorkStatus::Cancelled,
            _ => return None,
        })
    }

    pub fn is_open(&self) -> bool {
        matches!(self, WorkStatus::Todo | WorkStatus::InProgress)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionItem {
    pub id: Id,
    /// The meeting this came out of. Ownership lives here, not on the summary, so that
    /// regenerating a summary cannot take a user's owner, due date and status with it.
    pub meeting_id: Id,
    /// Which summary first surfaced this, if any — provenance, not ownership.
    pub summary_id: Option<Id>,
    pub text: String,
    /// Free-text owner as spoken or typed. Kept alongside `owner_person_id` rather than
    /// replaced by it: an owner named in a transcript often matches no known person.
    pub owner: Option<String>,
    pub owner_person_id: Option<Id>,
    pub due_at: Option<DateTime<Utc>>,
    pub status: WorkStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A workspace page. Block structure lives in `body` as serialized blocks; this crate
/// treats it as opaque text so the editor format can evolve without a schema migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: Id,
    pub project_id: Option<Id>,
    pub title: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When this note was moved to the trash, or `None` while it is live.
    ///
    /// Serialized always rather than skipped when absent: a client rendering a list of notes
    /// needs to distinguish "not trashed" from "this build does not report trashing", and an
    /// absent field cannot say which.
    pub deleted_at: Option<DateTime<Utc>>,
}

/// Reference to an issue in an external tracker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalRef {
    /// e.g. "linear", "jira", "github"
    pub provider: String,
    /// Provider-side identifier, e.g. "ENG-421".
    pub external_id: String,
    pub url: Option<String>,
}

/// Native lightweight ticket. Exists so the local product is useful with no tracker
/// connected; may mirror an external issue once integrations are enabled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub id: Id,
    pub project_id: Option<Id>,
    pub title: String,
    pub description: Option<String>,
    pub status: WorkStatus,
    pub owner: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub external: Option<ExternalRef>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftStatus {
    /// Generated, not yet seen by the user.
    Draft,
    /// User reviewed and approved for sending.
    Approved,
    Sent,
    Discarded,
}

impl DraftStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            DraftStatus::Draft => "draft",
            DraftStatus::Approved => "approved",
            DraftStatus::Sent => "sent",
            DraftStatus::Discarded => "discarded",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "draft" => DraftStatus::Draft,
            "approved" => DraftStatus::Approved,
            "sent" => DraftStatus::Sent,
            "discarded" => DraftStatus::Discarded,
            _ => return None,
        })
    }
}

/// A generated follow-up email.
///
/// Status starts at `Draft` and requires an explicit user transition to `Approved` before
/// anything may send it. Nothing in this codebase moves a draft straight to `Sent` — see
/// SECURITY.md.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailDraft {
    pub id: Id,
    pub meeting_id: Option<Id>,
    pub subject: String,
    pub body: String,
    pub recipients: Vec<String>,
    pub status: DraftStatus,
    /// Label for the tone/strategy variant, e.g. "concise recap".
    pub variant: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationChannel {
    Desktop,
    Push,
    Slack,
    Email,
    /// Batched into a periodic digest rather than delivered immediately.
    Digest,
}

impl NotificationChannel {
    pub fn as_str(&self) -> &'static str {
        match self {
            NotificationChannel::Desktop => "desktop",
            NotificationChannel::Push => "push",
            NotificationChannel::Slack => "slack",
            NotificationChannel::Email => "email",
            NotificationChannel::Digest => "digest",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "desktop" => NotificationChannel::Desktop,
            "push" => NotificationChannel::Push,
            "slack" => NotificationChannel::Slack,
            "email" => NotificationChannel::Email,
            "digest" => NotificationChannel::Digest,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub id: Id,
    /// Entity kind that triggered this, matching `graph::NodeKind` naming.
    pub source_kind: String,
    pub source_id: Id,
    pub recipient: String,
    pub channel: NotificationChannel,
    pub body: String,
    pub read_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl Notification {
    pub fn is_unread(&self) -> bool {
        self.read_at.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0)
            .single()
            .expect("valid timestamp")
    }

    #[test]
    fn meeting_duration_is_none_while_recording() {
        let m = Meeting {
            id: Id::new(),
            project_id: None,
            title: "Standup".into(),
            source: MeetingSource::Microphone,
            started_at: ts(1000),
            ended_at: None,
            series_id: None,
            created_at: ts(1000),
            updated_at: ts(1000),
        };
        assert!(m.is_recording());
        assert_eq!(m.duration_ms(), None);
    }

    #[test]
    fn meeting_duration_computed_once_ended() {
        let m = Meeting {
            id: Id::new(),
            project_id: None,
            title: "Standup".into(),
            source: MeetingSource::Combined,
            started_at: ts(1000),
            ended_at: Some(ts(1090)),
            series_id: None,
            created_at: ts(1000),
            updated_at: ts(1090),
        };
        assert!(!m.is_recording());
        assert_eq!(m.duration_ms(), Some(90_000));
    }

    #[test]
    fn work_status_round_trips() {
        for s in [
            WorkStatus::Todo,
            WorkStatus::InProgress,
            WorkStatus::Done,
            WorkStatus::Cancelled,
        ] {
            assert_eq!(WorkStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(WorkStatus::parse("nonsense"), None);
    }

    #[test]
    fn only_todo_and_in_progress_are_open() {
        assert!(WorkStatus::Todo.is_open());
        assert!(WorkStatus::InProgress.is_open());
        assert!(!WorkStatus::Done.is_open());
        assert!(!WorkStatus::Cancelled.is_open());
    }

    #[test]
    fn meeting_source_round_trips() {
        for s in [
            MeetingSource::Microphone,
            MeetingSource::SystemAudio,
            MeetingSource::Combined,
            MeetingSource::BrowserTab,
            MeetingSource::Import,
        ] {
            assert_eq!(MeetingSource::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn notification_channel_round_trips() {
        for c in [
            NotificationChannel::Desktop,
            NotificationChannel::Push,
            NotificationChannel::Slack,
            NotificationChannel::Email,
            NotificationChannel::Digest,
        ] {
            assert_eq!(NotificationChannel::parse(c.as_str()), Some(c));
        }
    }

    #[test]
    fn draft_status_round_trips() {
        for s in [
            DraftStatus::Draft,
            DraftStatus::Approved,
            DraftStatus::Sent,
            DraftStatus::Discarded,
        ] {
            assert_eq!(DraftStatus::parse(s.as_str()), Some(s));
        }
    }
}
