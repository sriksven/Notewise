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

#[cfg(test)]
mod tests {
    use super::*;

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
