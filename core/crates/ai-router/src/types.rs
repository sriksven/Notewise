//! Request and response types shared by every backend.
//!
//! These are deliberately provider-neutral: nothing here mentions Ollama, Anthropic, or any
//! other vendor. That is what lets a caller swap backends without changing its own code.

use serde::{Deserialize, Serialize};

/// A meeting transcript handed to a backend for analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptInput {
    pub title: String,
    /// Full transcript text, speaker-prefixed where diarization ran.
    pub text: String,
    /// Optional extra context, e.g. the project this meeting belongs to.
    pub context: Option<String>,
    /// A summary template's prompt, replacing the backend's default instruction.
    ///
    /// `None` means "use whatever this backend normally asks for", which is every caller that does
    /// not care. Kept separate from `context`: that is *material* about the meeting, this is an
    /// *instruction* about the output, and a backend has to treat them differently — one goes in the
    /// user message, the other in the system prompt.
    pub instructions: Option<String>,
}

impl TranscriptInput {
    /// Replace the backend's default instruction with a template's prompt.
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// The instruction to send: a template's prompt when one was chosen, else the backend's own.
    ///
    /// A method rather than each backend checking the field, so a backend that forgets is a backend
    /// that never compiled — and so every backend's default stays written where that backend is.
    pub fn system_prompt<'a>(&'a self, default: &'a str) -> &'a str {
        match self.instructions.as_deref() {
            Some(custom) if !custom.trim().is_empty() => custom,
            _ => default,
        }
    }

    pub fn new(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            text: text.into(),
            context: None,
            instructions: None,
        }
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }
}

/// A generated summary, tagged with the model that produced it so it can be
/// regenerated or audited later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryOutput {
    pub text: String,
    pub model: String,
}

/// A decision the model identified as having been reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedDecision {
    pub text: String,
    /// Why the decision was made, when the transcript makes it recoverable.
    pub reasoning: Option<String>,
}

/// A task the model identified as having been assigned or accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedActionItem {
    pub text: String,
    /// Owner as named in the transcript. Resolving that to a real user is the
    /// caller's job — this crate does not know about the identity model.
    pub owner: Option<String>,
    /// Due date as literally stated, e.g. "next Friday". Left unparsed on purpose:
    /// resolving it needs the meeting's timezone, which belongs to the caller.
    pub due_hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

/// A chat turn: grounding material plus conversation history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Transcripts, notes, or other material the answer should be grounded in.
    pub context: Vec<String>,
    /// Conversation so far. Must be non-empty and end with a user message.
    pub messages: Vec<ChatMessage>,
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            context: Vec::new(),
            messages,
        }
    }

    pub fn with_context(mut self, context: Vec<String>) -> Self {
        self.context = context;
        self
    }

    /// Whether this request is well-formed. Backends check this before spending a
    /// network call on a request the provider would reject anyway.
    pub fn is_valid(&self) -> bool {
        self.messages.last().is_some_and(|m| m.role == Role::User)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub text: String,
    pub model: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_builder_sets_context() {
        let input = TranscriptInput::new("Standup", "text").with_context("Project X");
        assert_eq!(input.context.as_deref(), Some("Project X"));
    }

    #[test]
    fn chat_request_must_end_with_a_user_message() {
        assert!(ChatRequest::new(vec![ChatMessage::user("hi")]).is_valid());

        assert!(!ChatRequest::new(vec![
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
        ])
        .is_valid());
    }

    #[test]
    fn empty_chat_request_is_invalid() {
        assert!(!ChatRequest::new(vec![]).is_valid());
    }

    #[test]
    fn roles_serialize_as_snake_case() {
        // The wire format matters: providers expect "user"/"assistant" exactly.
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
    }
}
