//! The instructions every backend sends for summaries and action items.
//!
//! One copy rather than one per backend. They were four near-identical strings, and the rules in
//! them are not provider-specific: a summary that invents a date, or an action item that turns a
//! colleague's family emergency into a task, is the same failure whichever model wrote it. Each
//! backend still adds what *is* specific to it — the JSON shape it asks for, or the schema it
//! constrains the reply to.
//!
//! # Why the rules are repeated after the transcript
//!
//! Measured against `llama3.1:8b` on a meeting where someone mentions a parent's surgery: with the
//! rules in the system prompt alone, two summaries in six still reported it and two action-item
//! lists in four contained "Take care of mom's surgery" with that person as owner. With a one-line
//! reminder after the transcript, none of twelve runs did. A small model weighs what it read last,
//! and the transcript is thousands of tokens of other text between the instruction and the answer.

use crate::types::TranscriptInput;

/// What a summary is for, and what stays out of it.
pub(crate) const SUMMARY: &str = "\
Summarize this meeting transcript. Lead with the outcome. Cover what was decided and what happens \
next, naming who owns each next step when the transcript says.

Stay with what was said. Repeat deadlines the way they were said and never work out a calendar \
date from them. If the transcript does not say something, leave it out rather than guessing.

Leave out small talk. Never mention anyone's health, family or other personal circumstances, even \
when it explains a delay or an absence — that was said to the people in the room, not for the \
record.";

/// Read after the transcript on the summary path. See the module docs for why it exists.
pub(crate) const SUMMARY_REMINDER: &str = "Write the summary now. It is for the record: leave out \
small talk and anything about a person's health, family or private life.";

/// What counts as an action item. A backend appends the reply shape it wants.
pub(crate) const ACTION_ITEMS: &str = "\
Extract the action items from this meeting: work someone committed to doing, or agreed to do when \
asked. Record the owner exactly as named in the transcript, or null when nobody took it on. Keep \
the due date exactly as it was said (\"by Friday\", \"next Wednesday\"), or null — never resolve \
it to a calendar date. Something personal is never an action item: nothing about anyone's health, \
family or private life.";

/// Read after the transcript on the action-item path.
pub(crate) const ACTION_ITEMS_REMINDER: &str = "List the action items now. Work commitments only: \
nothing about a person's health, family or private life.";

/// The user message: the meeting, then whatever should be read last.
///
/// `reminder` is empty where nothing needs repeating.
pub(crate) fn transcript_prompt(input: &TranscriptInput, reminder: &str) -> String {
    let mut prompt = format!("Meeting title: {}\n", input.title);
    if let Some(context) = &input.context {
        prompt.push_str(&format!("Context: {context}\n"));
    }
    prompt.push_str("\nTranscript:\n");
    prompt.push_str(&input.text);
    if !reminder.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(reminder);
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transcript_comes_before_the_reminder() {
        let prompt = transcript_prompt(
            &TranscriptInput::new("Sync", "Sam: I'll ship it.").with_context("Billing"),
            SUMMARY_REMINDER,
        );

        let transcript = prompt.find("Sam: I'll ship it.").expect("transcript");
        let reminder = prompt.find(SUMMARY_REMINDER).expect("reminder");
        assert!(transcript < reminder, "{prompt}");
        assert!(prompt.contains("Context: Billing"), "{prompt}");
    }

    #[test]
    fn no_reminder_means_nothing_after_the_transcript() {
        let prompt = transcript_prompt(&TranscriptInput::new("Sync", "text"), "");
        assert!(prompt.ends_with("text"), "{prompt:?}");
        assert!(!prompt.contains("Context:"), "{prompt}");
    }

    /// The two rules the real-model run showed being broken: an invented calendar date, and a
    /// personal matter reported as meeting content.
    #[test]
    fn the_summary_rules_cover_dates_and_personal_matters() {
        assert!(SUMMARY.contains("never work out a calendar date"));
        assert!(SUMMARY.contains("health, family"));
        assert!(SUMMARY_REMINDER.contains("health, family"));
    }

    #[test]
    fn the_action_item_rules_keep_deadlines_and_exclude_personal_matters() {
        assert!(ACTION_ITEMS.contains("exactly as it was said"));
        assert!(ACTION_ITEMS.contains("Something personal is never an action item"));
        assert!(ACTION_ITEMS_REMINDER.contains("Work commitments only"));
    }
}
