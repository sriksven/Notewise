//! Per-request backend selection.
//!
//! Everything here is pure. Selection has to be cheap enough to run before every model call, so
//! it looks only at facts already in hand — which trait method was invoked, how big the input is,
//! what time it is — and never at a network or a model.

use serde::{Deserialize, Serialize};

/// Which kind of work a request is.
///
/// Derived from the `AiBackend` method being called, which makes it free and exact — the trait
/// already separates a two-word title from a ninety-minute summary, and that separation is most
/// of what routing exists to exploit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Summarize,
    ExtractDecisions,
    ExtractActionItems,
    Chat,
}

impl TaskKind {
    pub const ALL: &'static [TaskKind] = &[
        TaskKind::Summarize,
        TaskKind::ExtractDecisions,
        TaskKind::ExtractActionItems,
        TaskKind::Chat,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            TaskKind::Summarize => "summarize",
            TaskKind::ExtractDecisions => "extract_decisions",
            TaskKind::ExtractActionItems => "extract_action_items",
            TaskKind::Chat => "chat",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

/// Roughly how many tokens a string is, without a tokenizer.
///
/// Four bytes per token. An exact count needs a tokenizer per model family, and the predicates
/// this feeds exist to tell a title from a transcript — a decision no plausible tokenizer
/// disagreement changes.
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Everything selection is allowed to look at.
///
/// Built once per request by the caller that knows the shape of the input, so a [`Predicate`]
/// never has to care whether it is judging a transcript or a chat history.
#[derive(Debug, Clone)]
pub struct RequestFacts {
    pub task: TaskKind,
    pub estimated_tokens: usize,
    /// Local hour, 0..=23. Injected rather than read from the clock so selection stays pure.
    pub hour_of_day: u8,
    /// The text a keyword predicate searches: title and context for a transcript, the last user
    /// message for a chat. Lowercased once here so every predicate does not repeat it.
    pub text: String,
}

impl RequestFacts {
    pub fn for_transcript(
        task: TaskKind,
        title: &str,
        body: &str,
        context: Option<&str>,
        hour_of_day: u8,
    ) -> Self {
        let extra = context.unwrap_or_default();
        Self {
            task,
            estimated_tokens: estimate_tokens(title)
                + estimate_tokens(body)
                + estimate_tokens(extra),
            hour_of_day,
            text: format!("{title} {extra}").to_lowercase(),
        }
    }

    pub fn for_chat(context: &[String], last_user_message: &str, hour_of_day: u8) -> Self {
        let context_tokens: usize = context.iter().map(|c| estimate_tokens(c)).sum();
        Self {
            task: TaskKind::Chat,
            estimated_tokens: context_tokens + estimate_tokens(last_user_message),
            hour_of_day,
            text: last_user_message.to_lowercase(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_estimated_from_length_not_tokenized() {
        // Four characters per token is wrong in the third significant figure and right about
        // the only thing a predicate asks: is this a title or a transcript.
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens(&"x".repeat(4000)), 1000);
    }

    #[test]
    fn facts_for_a_transcript_count_title_text_and_context() {
        let facts = RequestFacts::for_transcript(
            TaskKind::Summarize,
            "Standup",
            &"x".repeat(400),
            Some("Platform"),
            9,
        );

        assert_eq!(facts.task, TaskKind::Summarize);
        assert!(facts.estimated_tokens >= 100, "{}", facts.estimated_tokens);
        assert_eq!(facts.hour_of_day, 9);
        assert!(facts.text.contains("standup"));
    }

    #[test]
    fn task_kinds_round_trip_through_their_wire_names() {
        for kind in TaskKind::ALL {
            assert_eq!(TaskKind::parse(kind.as_str()), Some(*kind), "{kind:?}");
        }
    }

    #[test]
    fn an_unknown_task_name_is_none_not_a_default() {
        // A rule stored against a task this build does not know must not silently match
        // something else.
        assert_eq!(TaskKind::parse("transcribe"), None);
    }
}
