//! Markdown export.
//!
//! Lives in `storage` because it reads across every repository and nothing else would own it
//! without a crate whose only job is string formatting. It writes no SQL of its own — it
//! composes repository calls, so the rule that SQL stays in this crate is unaffected.
//!
//! Markdown specifically: it is the format that survives. A user who stops using Notewise
//! should still be able to read their meetings, and an export nobody can open without the
//! original app is not really an export.

use crate::db::Database;
use crate::error::Result;
use crate::id::Id;
use crate::models::WorkStatus;
use crate::repositories::{MeetingRepository, SummaryRepository};

/// What to include in an exported meeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportOptions {
    pub include_summary: bool,
    pub include_decisions: bool,
    pub include_action_items: bool,
    pub include_transcript: bool,
    /// Prefix each transcript line with its timestamp.
    pub include_timestamps: bool,
}

impl Default for ExportOptions {
    /// Everything except timestamps.
    ///
    /// Timestamps are off by default because the common use of an export is pasting the
    /// summary somewhere a reader does not care that a sentence began at 04:12.
    fn default() -> Self {
        Self {
            include_summary: true,
            include_decisions: true,
            include_action_items: true,
            include_transcript: true,
            include_timestamps: false,
        }
    }
}

impl ExportOptions {
    /// Summary, decisions, and action items — no transcript.
    ///
    /// What someone actually pastes into a follow-up message.
    pub fn brief() -> Self {
        Self {
            include_transcript: false,
            ..Default::default()
        }
    }

    /// The transcript alone, with timestamps.
    pub fn transcript_only() -> Self {
        Self {
            include_summary: false,
            include_decisions: false,
            include_action_items: false,
            include_transcript: true,
            include_timestamps: true,
        }
    }
}

fn timestamp(ms: i64) -> String {
    let total = (ms / 1000).max(0);
    let s = total % 60;
    let m = (total / 60) % 60;
    let h = total / 3600;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Render a meeting as Markdown.
pub fn meeting_to_markdown(
    db: &Database,
    meeting_id: Id,
    options: ExportOptions,
) -> Result<String> {
    let meetings = MeetingRepository::new(db);
    let meeting = meetings.get(meeting_id)?;

    let mut out = String::new();

    out.push_str(&format!("# {}\n\n", meeting.title));
    out.push_str(&format!(
        "**Date:** {}\n",
        meeting.started_at.format("%Y-%m-%d %H:%M UTC")
    ));

    match meeting.duration_ms() {
        Some(ms) => out.push_str(&format!("**Duration:** {}\n", timestamp(ms))),
        None => out.push_str("**Duration:** still recording\n"),
    }
    out.push('\n');

    let summaries = SummaryRepository::new(db);
    let summary = summaries.latest_for_meeting(meeting_id)?;

    if let Some(summary) = &summary {
        if options.include_summary {
            out.push_str("## Summary\n\n");
            out.push_str(summary.text.trim());
            out.push_str("\n\n");
        }

        if options.include_decisions {
            let decisions = summaries.decisions(summary.id)?;
            if !decisions.is_empty() {
                out.push_str("## Decisions\n\n");
                for decision in decisions {
                    out.push_str(&format!("- {}", decision.text.trim()));
                    if let Some(reasoning) =
                        decision.reasoning.as_ref().filter(|r| !r.trim().is_empty())
                    {
                        out.push_str(&format!("\n  - _{}_", reasoning.trim()));
                    }
                    out.push('\n');
                }
                out.push('\n');
            }
        }

        if options.include_action_items {
            let items = summaries.action_items(summary.id)?;
            if !items.is_empty() {
                out.push_str("## Action items\n\n");
                for item in items {
                    // GitHub-flavoured task list: a done item exports as ticked, so the
                    // export reflects state rather than flattening it.
                    let checkbox = if item.status == WorkStatus::Done {
                        "[x]"
                    } else {
                        "[ ]"
                    };
                    out.push_str(&format!("- {checkbox} {}", item.text.trim()));

                    if let Some(owner) = &item.owner {
                        out.push_str(&format!(" — **{owner}**"));
                    }
                    if let Some(due) = item.due_at {
                        out.push_str(&format!(" (due {})", due.format("%Y-%m-%d")));
                    }
                    if item.status == WorkStatus::Cancelled {
                        out.push_str(" _(cancelled)_");
                    }
                    out.push('\n');
                }
                out.push('\n');
            }
        }
    } else if options.include_summary {
        out.push_str("## Summary\n\n_This meeting has not been summarized._\n\n");
    }

    if options.include_transcript {
        let segments = meetings.segments(meeting_id)?;
        out.push_str("## Transcript\n\n");

        if segments.is_empty() {
            out.push_str("_No transcript._\n");
        } else {
            // `Option<Option<String>>` rather than `Option<String>`: the outer `None` means
            // "no heading written yet", which is distinct from a segment whose speaker is
            // unknown. Collapsing the two loses the heading on an undiarized transcript,
            // because the first segment would compare equal to the initial state.
            let mut last_speaker: Option<Option<String>> = None;

            for segment in segments {
                let changed = last_speaker.as_ref() != Some(&segment.speaker);

                if changed {
                    if last_speaker.is_some() {
                        out.push('\n');
                    }
                    let name = segment
                        .speaker
                        .clone()
                        .unwrap_or_else(|| "Unattributed".into());
                    if options.include_timestamps {
                        out.push_str(&format!("**{name}** ({})\n", timestamp(segment.start_ms)));
                    } else {
                        out.push_str(&format!("**{name}**\n"));
                    }
                    last_speaker = Some(segment.speaker.clone());
                }

                out.push_str(&format!("{}\n", segment.text.trim()));
            }
        }
    }

    // Provenance: which model wrote the summary, so a reader can weigh it.
    if let Some(summary) = summary.filter(|_| options.include_summary) {
        out.push_str(&format!(
            "\n---\n\n_Summarized by {} via Notewise._\n",
            summary.model
        ));
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::MeetingSource;
    use crate::repositories::{
        NewActionItem, NewDecision, NewMeeting, NewSummary, NewTranscriptSegment,
    };
    use chrono::{TimeZone, Utc};

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn ts(secs: i64) -> chrono::DateTime<Utc> {
        Utc.timestamp_opt(secs, 0)
            .single()
            .expect("valid timestamp")
    }

    /// A meeting with a transcript, a summary, a decision, and two action items.
    fn full_meeting(db: &Database) -> Id {
        let meetings = MeetingRepository::new(db);
        let meeting = meetings
            .create(NewMeeting {
                project_id: None,
                title: "Infra sync".into(),
                source: MeetingSource::Combined,
                started_at: ts(1_700_000_000),
            })
            .unwrap();

        for (speaker, text, start) in [
            ("Alex", "Where did we land on Postgres?", 0),
            ("Alex", "We should move before launch.", 4000),
            ("Sam", "I'll draft the plan by Friday.", 9000),
        ] {
            meetings
                .add_segment(NewTranscriptSegment {
                    meeting_id: meeting.id,
                    speaker: Some(speaker.into()),
                    text: text.into(),
                    start_ms: start,
                    end_ms: start + 3000,
                    confidence: None,
                })
                .unwrap();
        }

        meetings.end(meeting.id, ts(1_700_000_600)).unwrap();

        let summaries = SummaryRepository::new(db);
        let summary = summaries
            .create(NewSummary {
                meeting_id: meeting.id,
                text: "The team agreed to migrate to Postgres before launch.".into(),
                model: "llama3.1".into(),
                template_id: None,
            })
            .unwrap();

        summaries
            .add_decision(NewDecision {
                meeting_id: meeting.id,
                summary_id: Some(summary.id),
                text: "Migrate to Postgres".into(),
                reasoning: Some("SQLite will not scale past launch".into()),
                decided_at: None,
            })
            .unwrap();

        let done = summaries
            .add_action_item(NewActionItem {
                meeting_id: meeting.id,
                summary_id: Some(summary.id),
                text: "Benchmark the FTS index".into(),
                owner: Some("Alex".into()),
                owner_person_id: None,
                due_at: None,
            })
            .unwrap();
        summaries
            .set_action_item_status(done.id, WorkStatus::Done)
            .unwrap();

        summaries
            .add_action_item(NewActionItem {
                meeting_id: meeting.id,
                summary_id: Some(summary.id),
                text: "Draft the migration plan".into(),
                owner: Some("Sam".into()),
                owner_person_id: None,
                due_at: Some(ts(1_700_400_000)),
            })
            .unwrap();

        meeting.id
    }

    #[test]
    fn a_full_export_contains_every_section() {
        let db = db();
        let id = full_meeting(&db);
        let md = meeting_to_markdown(&db, id, ExportOptions::default()).unwrap();

        assert!(md.starts_with("# Infra sync"));
        for heading in [
            "## Summary",
            "## Decisions",
            "## Action items",
            "## Transcript",
        ] {
            assert!(md.contains(heading), "missing {heading}\n{md}");
        }
    }

    #[test]
    fn completed_action_items_export_as_ticked() {
        let db = db();
        let md = meeting_to_markdown(&db, full_meeting(&db), ExportOptions::default()).unwrap();

        assert!(md.contains("- [x] Benchmark the FTS index"), "{md}");
        assert!(md.contains("- [ ] Draft the migration plan"), "{md}");
    }

    #[test]
    fn owners_and_due_dates_are_rendered() {
        let db = db();
        let md = meeting_to_markdown(&db, full_meeting(&db), ExportOptions::default()).unwrap();

        assert!(md.contains("**Alex**"), "{md}");
        assert!(md.contains("(due 2023-11-19)"), "{md}");
    }

    #[test]
    fn decision_reasoning_is_nested_under_its_decision() {
        let db = db();
        let md = meeting_to_markdown(&db, full_meeting(&db), ExportOptions::default()).unwrap();

        assert!(md.contains("- Migrate to Postgres"), "{md}");
        assert!(
            md.contains("  - _SQLite will not scale past launch_"),
            "{md}"
        );
    }

    #[test]
    fn consecutive_lines_from_one_speaker_share_a_heading() {
        // Repeating the speaker on every line makes a transcript much harder to read back.
        let db = db();
        let md = meeting_to_markdown(&db, full_meeting(&db), ExportOptions::default()).unwrap();

        let transcript = md
            .split("## Transcript")
            .nth(1)
            .expect("transcript section");
        assert_eq!(
            transcript.matches("**Alex**").count(),
            1,
            "Alex spoke twice in a row and should get one heading:\n{transcript}"
        );
        assert!(
            transcript.contains("Where did we land on Postgres?\nWe should move before launch.")
        );
    }

    #[test]
    fn the_model_is_recorded_so_a_reader_can_weigh_the_summary() {
        let db = db();
        let md = meeting_to_markdown(&db, full_meeting(&db), ExportOptions::default()).unwrap();
        assert!(
            md.contains("_Summarized by llama3.1 via Notewise._"),
            "{md}"
        );
    }

    #[test]
    fn timestamps_are_off_by_default_and_opt_in() {
        let db = db();
        let id = full_meeting(&db);

        let plain = meeting_to_markdown(&db, id, ExportOptions::default()).unwrap();
        assert!(!plain.contains("(00:00)"), "{plain}");

        let stamped = meeting_to_markdown(&db, id, ExportOptions::transcript_only()).unwrap();
        assert!(stamped.contains("**Alex** (00:00)"), "{stamped}");
        assert!(stamped.contains("**Sam** (00:09)"), "{stamped}");
    }

    #[test]
    fn a_brief_export_omits_the_transcript() {
        let db = db();
        let md = meeting_to_markdown(&db, full_meeting(&db), ExportOptions::brief()).unwrap();

        assert!(md.contains("## Summary"));
        assert!(!md.contains("## Transcript"), "{md}");
    }

    #[test]
    fn a_transcript_only_export_omits_the_summary() {
        let db = db();
        let md =
            meeting_to_markdown(&db, full_meeting(&db), ExportOptions::transcript_only()).unwrap();

        assert!(md.contains("## Transcript"));
        assert!(!md.contains("## Summary"), "{md}");
        assert!(!md.contains("## Decisions"), "{md}");
    }

    #[test]
    fn an_unsummarized_meeting_says_so_rather_than_omitting_the_section() {
        let db = db();
        let meeting = MeetingRepository::new(&db)
            .create(NewMeeting {
                project_id: None,
                title: "Unprocessed".into(),
                source: MeetingSource::Import,
                started_at: ts(1_700_000_000),
            })
            .unwrap();

        let md = meeting_to_markdown(&db, meeting.id, ExportOptions::default()).unwrap();

        assert!(
            md.contains("_This meeting has not been summarized._"),
            "{md}"
        );
        assert!(md.contains("_No transcript._"), "{md}");
    }

    #[test]
    fn a_still_recording_meeting_exports_without_a_duration() {
        let db = db();
        let meeting = MeetingRepository::new(&db)
            .create(NewMeeting {
                project_id: None,
                title: "Live".into(),
                source: MeetingSource::Microphone,
                started_at: ts(1_700_000_000),
            })
            .unwrap();

        let md = meeting_to_markdown(&db, meeting.id, ExportOptions::default()).unwrap();
        assert!(md.contains("**Duration:** still recording"), "{md}");
    }

    #[test]
    fn an_unknown_meeting_is_an_error_not_an_empty_document() {
        let db = db();
        assert!(meeting_to_markdown(&db, Id::new(), ExportOptions::default()).is_err());
    }

    #[test]
    fn unattributed_speech_is_labelled_rather_than_left_blank() {
        let db = db();
        let meetings = MeetingRepository::new(&db);
        let meeting = meetings
            .create(NewMeeting {
                project_id: None,
                title: "No diarization".into(),
                source: MeetingSource::Microphone,
                started_at: ts(1_700_000_000),
            })
            .unwrap();
        meetings
            .add_segment(NewTranscriptSegment {
                meeting_id: meeting.id,
                speaker: None,
                text: "Someone said this.".into(),
                start_ms: 0,
                end_ms: 2000,
                confidence: None,
            })
            .unwrap();

        let md = meeting_to_markdown(&db, meeting.id, ExportOptions::default()).unwrap();
        assert!(md.contains("**Unattributed**"), "{md}");
    }

    #[test]
    fn durations_over_an_hour_include_hours() {
        assert_eq!(timestamp(0), "00:00");
        assert_eq!(timestamp(65_000), "01:05");
        assert_eq!(timestamp(3_661_000), "1:01:01");
        assert_eq!(timestamp(-5000), "00:00");
    }
}

/// Render a draft as an RFC 5322 message, for a mail client to open.
///
/// # Why this exists beside the two connectors
///
/// Putting a draft in Gmail or Outlook needs an account connected, and most people trying Notewise
/// for the first time have not connected one. They still have a mail client. An `.eml` file is what
/// every one of them can open, and opening it puts the draft in front of the user with the recipients
/// and subject already filled in — the same end state the connectors reach, by a route that needs no
/// vendor, no token, and no review.
///
/// # Why the envelope is all that is new
///
/// The body came from the email generator, which already drafts and never sends. This adds headers.
/// There is deliberately no `Date` and no `Message-ID`: a client composing from a draft supplies its
/// own, and a stale one from whenever the draft was generated would be wrong by the time it is sent.
///
/// # What is not attempted
///
/// No MIME multipart, no attachments, no HTML alternative. The body is plain text, so it is declared
/// as plain text — and a message that says `text/plain` and means it needs no boundary machinery.
pub fn draft_to_eml(draft: &crate::models::EmailDraft) -> String {
    let mut out = String::new();

    let recipients: Vec<&str> = draft
        .recipients
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .collect();

    if !recipients.is_empty() {
        out.push_str(&format!("To: {}\r\n", recipients.join(", ")));
    }

    out.push_str(&format!("Subject: {}\r\n", header_value(&draft.subject)));
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    // `X-Unsent: 1` is what tells Outlook and Apple Mail to open this as a *draft* to edit rather
    // than as a received message to read. Without it the file opens read-only and the user has to
    // copy the text out, which defeats the point.
    out.push_str("X-Unsent: 1\r\n");
    out.push_str("\r\n");

    // Bare newlines become CRLF: the format requires it, and a client that is strict about it shows
    // one long paragraph otherwise.
    out.push_str(&draft.body.replace("\r\n", "\n").replace('\n', "\r\n"));

    out
}

/// A header value with the characters that would end the header removed.
///
/// A newline inside a subject is header injection — it would let a subject add its own headers, and
/// the subject here is model-generated text. Folding it correctly would be the other answer;
/// stripping is the one with no way to be subtly wrong.
fn header_value(raw: &str) -> String {
    let flattened: String = raw
        .chars()
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .collect();

    // Whitespace collapsed as well, so a stripped CRLF leaves one space rather than two and the
    // subject a client shows does not look like a formatting bug.
    flattened.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod eml_tests {
    use super::*;
    use crate::models::{DraftStatus, EmailDraft};
    use crate::Id;
    use chrono::Utc;

    fn draft(subject: &str, body: &str, recipients: &[&str]) -> EmailDraft {
        EmailDraft {
            id: Id::new(),
            meeting_id: None,
            subject: subject.into(),
            body: body.into(),
            recipients: recipients.iter().map(|r| (*r).to_string()).collect(),
            status: DraftStatus::Draft,
            variant: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn a_draft_renders_as_a_message_a_client_can_open() {
        let rendered = draft_to_eml(&draft(
            "Follow-up: Platform standup",
            "Here is what we agreed.\nShip on Friday.",
            &["priya@example.com", "sam@example.com"],
        ));

        assert!(rendered.contains("To: priya@example.com, sam@example.com\r\n"));
        assert!(rendered.contains("Subject: Follow-up: Platform standup\r\n"));
        assert!(rendered.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(rendered.ends_with("Ship on Friday."));
    }

    /// Without this the file opens read-only and the user has to copy the text out, which defeats
    /// the point of generating a draft.
    #[test]
    fn it_opens_as_a_draft_rather_than_a_received_message() {
        let rendered = draft_to_eml(&draft("Subject", "Body", &["a@b.com"]));
        assert!(rendered.contains("X-Unsent: 1\r\n"), "{rendered}");
    }

    /// The headers end where the body begins, and exactly once.
    #[test]
    fn the_headers_are_separated_from_the_body_by_a_blank_line() {
        let rendered = draft_to_eml(&draft("Subject", "Body", &["a@b.com"]));
        let (headers, body) = rendered
            .split_once("\r\n\r\n")
            .expect("a blank line separates them");

        assert!(headers.contains("Subject: Subject"));
        assert_eq!(body, "Body");
    }

    /// The format requires CRLF, and a strict client shows one long paragraph without it.
    #[test]
    fn newlines_in_the_body_become_crlf() {
        let rendered = draft_to_eml(&draft("S", "one\ntwo\nthree", &["a@b.com"]));
        let body = rendered.split_once("\r\n\r\n").expect("a body").1;
        assert_eq!(body, "one\r\ntwo\r\nthree");
    }

    /// And a body that already had CRLF must not end up with doubled carriage returns.
    #[test]
    fn a_body_that_is_already_crlf_is_not_doubled() {
        let rendered = draft_to_eml(&draft("S", "one\r\ntwo", &["a@b.com"]));
        let body = rendered.split_once("\r\n\r\n").expect("a body").1;
        assert_eq!(body, "one\r\ntwo");
        assert!(!body.contains("\r\r"));
    }

    /// The subject is model-generated text, and a newline in a header is header injection.
    #[test]
    fn a_newline_in_the_subject_cannot_add_a_header() {
        let rendered = draft_to_eml(&draft(
            "Follow-up\r\nBcc: everyone@example.com",
            "Body",
            &["a@b.com"],
        ));

        // The property is that it is not a *header*, not that the text is absent: "Bcc:" sitting
        // inside a subject value is harmless, and asserting it away would be asserting the wrong
        // thing. What must not happen is a line beginning with it.
        let headers = rendered.split_once("\r\n\r\n").expect("a body").0;
        assert!(
            !headers
                .lines()
                .any(|line| line.to_lowercase().starts_with("bcc:")),
            "a subject smuggled a header in: {headers}"
        );
        assert!(headers.contains("Subject: Follow-up Bcc: everyone@example.com"));
    }

    /// A draft with nobody to send it to still opens; the client asks for a recipient.
    #[test]
    fn a_draft_with_no_recipients_omits_the_header_rather_than_writing_an_empty_one() {
        let rendered = draft_to_eml(&draft("Subject", "Body", &[]));
        assert!(!rendered.contains("To:"), "{rendered}");
        assert!(rendered.contains("Subject: Subject"));
    }

    #[test]
    fn blank_recipients_are_dropped() {
        let rendered = draft_to_eml(&draft("S", "B", &["a@b.com", "   ", ""]));
        assert!(rendered.contains("To: a@b.com\r\n"), "{rendered}");
    }

    /// A subject in any script, because a follow-up is written in the language of the meeting.
    #[test]
    fn a_non_ascii_subject_survives() {
        let rendered = draft_to_eml(&draft("Suivi : réunion d'équipe", "Corps", &["a@b.com"]));
        assert!(rendered.contains("Suivi : réunion d'équipe"), "{rendered}");
        assert!(rendered.contains("charset=utf-8"));
    }

    /// No Date and no Message-ID: a client composing from a draft supplies its own, and a stale one
    /// from whenever this was generated would be wrong by the time it is sent.
    #[test]
    fn no_timestamp_is_baked_in() {
        let rendered = draft_to_eml(&draft("S", "B", &["a@b.com"]));
        assert!(!rendered.contains("Date:"), "{rendered}");
        assert!(!rendered.contains("Message-ID:"), "{rendered}");
    }
}
