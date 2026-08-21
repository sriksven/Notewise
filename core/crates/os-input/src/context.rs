//! What is on screen, reduced to text before it leaves this crate.
//!
//! # Why text and not pixels
//!
//! Returning an image would push the vision-versus-OCR decision into every consumer and make the
//! privacy question — what exactly left the machine — depend on which feature was calling. One
//! text-shaped contract means one answerable question.
//!
//! # Why structured text wins over recognised text
//!
//! The accessibility API returns what an application says its field contains. Optical recognition
//! returns a guess about what some pixels look like. When both are available the first is correct
//! and the second is approximate, so there is no case for blending them — and a prompt built from
//! recognised text when the real text was available is worse output for more effort.
//!
//! # Why an empty context is a success
//!
//! A user on an empty desktop with nothing focused has no context, and that is an answer. Returning
//! an error would make every consumer handle "nothing to see" as a failure and say so to the user,
//! which is wrong twice.

use serde::{Deserialize, Serialize};

/// What could be learned about the user's current surroundings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenContext {
    /// The frontmost application's name.
    pub app: Option<String>,
    pub window_title: Option<String>,
    /// What the user has highlighted. The strongest signal of intent there is.
    pub selection: Option<String>,
    /// The whole contents of the focused field.
    pub focused_text: Option<String>,
    /// Text read from pixels, when nothing structured was available.
    pub recognised_text: Option<String>,
}

/// How much context may go into a prompt.
///
/// A screen's worth of text is both a cost and a disclosure. Two thousand characters is enough for
/// the paragraph somebody is working on and not enough to be an accidental document upload.
pub const PROMPT_LIMIT: usize = 2_000;

impl ScreenContext {
    /// Whether anything was learned at all.
    pub fn is_empty(&self) -> bool {
        self.app.is_none()
            && self.window_title.is_none()
            && self.selection.is_none()
            && self.focused_text.is_none()
            && self.recognised_text.is_none()
    }

    /// The most trustworthy body of text available.
    ///
    /// A selection beats the whole field, because highlighting something is the clearest statement
    /// of what the user means. The field beats recognised text, because it is what the application
    /// says rather than what the pixels suggest.
    pub fn best_text(&self) -> Option<&str> {
        [
            self.selection.as_deref(),
            self.focused_text.as_deref(),
            self.recognised_text.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|text| !text.trim().is_empty())
    }

    /// Whether the text on offer came from pixels rather than from the application.
    ///
    /// Worth surfacing: an answer grounded in recognised text can be wrong because the recognition
    /// was, and a consumer may want to say so.
    pub fn is_recognised(&self) -> bool {
        self.selection
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
            && self
                .focused_text
                .as_deref()
                .is_none_or(|s| s.trim().is_empty())
            && self
                .recognised_text
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
    }

    /// One block, for putting in front of a model.
    ///
    /// Empty when there is nothing to say, so a caller can pass it straight through without a
    /// special case for "no context" — the model sees no context section rather than an empty one.
    pub fn to_prompt(&self, limit: usize) -> String {
        let mut lines = Vec::new();

        if let Some(app) = self.app.as_deref().filter(|a| !a.trim().is_empty()) {
            lines.push(format!("Application: {app}"));
        }
        if let Some(title) = self
            .window_title
            .as_deref()
            .filter(|t| !t.trim().is_empty())
        {
            lines.push(format!("Window: {title}"));
        }

        if let Some(text) = self.best_text() {
            let label = if self.selection.as_deref().is_some_and(|s| s == text) {
                "Selected text"
            } else if self.is_recognised() {
                // Labelled so a model — and anyone reading a trace — knows this is a guess.
                "Text read from the screen (may contain recognition errors)"
            } else {
                "Text in the focused field"
            };
            lines.push(format!("{label}:\n{}", truncate(text, limit)));
        }

        lines.join("\n")
    }
}

/// Cut a string to a character budget without splitting a character.
///
/// Byte slicing would panic on the multibyte boundary in the middle of a transcript, and a
/// dictation surface sees every script a person speaks.
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }

    let kept: String = text.chars().take(limit).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ScreenContext {
        ScreenContext {
            app: Some("Mail".into()),
            window_title: Some("Re: Q3 planning".into()),
            ..Default::default()
        }
    }

    /// The rule A4 turns on: the application's own answer beats a guess about pixels.
    #[test]
    fn structured_text_is_preferred_over_recognised_text() {
        let mut with_both = context();
        with_both.focused_text = Some("what the field says".into());
        with_both.recognised_text = Some("wh4t the pixels suggest".into());

        assert_eq!(with_both.best_text(), Some("what the field says"));
        assert!(!with_both.is_recognised());
        assert!(!with_both.to_prompt(PROMPT_LIMIT).contains("pixels"));
    }

    /// Highlighting something is the clearest statement of what the user means.
    #[test]
    fn a_selection_beats_the_whole_field() {
        let mut with_both = context();
        with_both.selection = Some("this sentence".into());
        with_both.focused_text = Some("this sentence, and four paragraphs around it".into());

        assert_eq!(with_both.best_text(), Some("this sentence"));
        assert!(with_both.to_prompt(PROMPT_LIMIT).contains("Selected text"));
    }

    /// And when only pixels were available, the prompt says so — an answer grounded in recognised
    /// text can be wrong because the recognition was.
    #[test]
    fn recognised_text_is_labelled_as_a_guess() {
        let mut only_ocr = context();
        only_ocr.recognised_text = Some("wh4t the pixels suggest".into());

        assert!(only_ocr.is_recognised());
        let prompt = only_ocr.to_prompt(PROMPT_LIMIT);
        assert!(prompt.contains("recognition errors"), "{prompt}");
    }

    /// Returning an error here would make every consumer treat "nothing to see" as a failure and
    /// say so to the user, which is wrong twice.
    #[test]
    fn an_empty_context_is_a_valid_result() {
        let nothing = ScreenContext::default();
        assert!(nothing.is_empty());
        assert_eq!(nothing.best_text(), None);
        assert_eq!(nothing.to_prompt(PROMPT_LIMIT), "");
        assert!(!nothing.is_recognised());
    }

    /// A window title with no text in the field is still context worth having.
    #[test]
    fn a_context_with_only_a_window_is_not_empty() {
        let just_a_window = context();
        assert!(!just_a_window.is_empty());
        assert_eq!(just_a_window.best_text(), None);

        let prompt = just_a_window.to_prompt(PROMPT_LIMIT);
        assert!(prompt.contains("Mail"));
        assert!(prompt.contains("Re: Q3 planning"));
    }

    /// Whitespace is not content. A field holding spaces must not be offered as context.
    #[test]
    fn blank_text_is_not_text() {
        let mut blank = context();
        blank.focused_text = Some("   \n  ".into());
        assert_eq!(blank.best_text(), None);
        assert!(!blank.to_prompt(PROMPT_LIMIT).contains("focused field"));
    }

    #[test]
    fn blank_fields_are_left_out_rather_than_labelled_empty() {
        let blank = ScreenContext {
            app: Some("  ".into()),
            window_title: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(blank.to_prompt(PROMPT_LIMIT), "");
    }

    /// A screen's worth of text is both a cost and a disclosure.
    #[test]
    fn context_is_capped() {
        let huge = ScreenContext {
            focused_text: Some("x".repeat(10_000)),
            ..Default::default()
        };

        let prompt = huge.to_prompt(PROMPT_LIMIT);
        assert!(
            prompt.chars().count() < PROMPT_LIMIT + 200,
            "{}",
            prompt.len()
        );
        assert!(prompt.ends_with('…'), "the cut is visible");
    }

    /// A dictation surface sees every script a person speaks, so byte slicing would panic.
    #[test]
    fn the_cut_lands_on_a_character_boundary() {
        let multibyte = ScreenContext {
            focused_text: Some("日本語のテキスト".repeat(500)),
            ..Default::default()
        };

        let prompt = multibyte.to_prompt(10);
        assert!(prompt.contains("日本語のテキ"), "{prompt}");
    }

    #[test]
    fn text_under_the_limit_is_untouched() {
        assert_eq!(truncate("short", 100), "short");
        assert!(!truncate("short", 100).ends_with('…'));
    }

    #[test]
    fn a_context_round_trips_through_json() {
        let mut full = context();
        full.selection = Some("highlighted".into());

        let json = serde_json::to_string(&full).expect("serializes");
        assert_eq!(
            serde_json::from_str::<ScreenContext>(&json).expect("deserializes"),
            full
        );
    }
}
