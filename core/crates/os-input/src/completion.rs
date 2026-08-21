//! When to suggest a continuation, and what counts as one.
//!
//! # What inline completion can and cannot be on macOS
//!
//! It cannot be ghost text in somebody else's text field. The accessibility API lets an app read a
//! field's value and replace its selection; it has nothing that draws unaccepted text inside another
//! process's view. Every editor that shows greyed-out suggestions is drawing them itself, in its own
//! window, into its own buffer.
//!
//! So what is achievable here is a suggestion shown in *our* window, accepted with a key, and
//! inserted at the caret through the ordinary insertion path. That is a real feature and it is not
//! the feature the design sketched, and saying so is better than shipping something that renders
//! nothing and calling it ghost text.
//!
//! # Why the policy is here and pure
//!
//! Everything expensive about completion is a judgement call about *when*: too eager and it is a
//! model call per keystroke, too slow and the suggestion arrives after the sentence is finished.
//! None of that needs a keyboard to test.

use serde::{Deserialize, Serialize};

/// What a keystroke monitor has seen.
///
/// Timing and a count, and deliberately nothing else. There is no field here that could hold a key,
/// which is the whole argument for a feature that asks for Input Monitoring at all — see
/// `native::keystrokes`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypingActivity {
    pub running: bool,
    /// Milliseconds since the Unix epoch, or `None` if nothing has been typed since it started.
    pub last_keystroke_ms: Option<i64>,
    pub keystrokes: u64,
}

/// When a suggestion is worth asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionPolicy {
    /// How long the typing has to have stopped.
    ///
    /// Long enough that it is a pause and not the gap between two words. A fast typist leaves
    /// eighty milliseconds between keys; a person thinking leaves half a second.
    pub pause_ms: i64,
    /// How much text there has to be before a continuation means anything.
    ///
    /// Completing an empty field is guessing what somebody is about to write, which is a different
    /// and much worse product.
    pub min_chars: usize,
    /// The shortest gap between two asks.
    ///
    /// The rate limit. Without it a slow model plus a hesitant typist produces a queue of requests
    /// whose answers all arrive stale.
    pub min_gap_ms: i64,
    /// Above this, the field is a document rather than a sentence being written.
    pub max_chars: usize,
}

impl Default for CompletionPolicy {
    fn default() -> Self {
        Self {
            pause_ms: 600,
            min_chars: 12,
            min_gap_ms: 2_000,
            max_chars: 4_000,
        }
    }
}

/// What to do right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Ask the model.
    Ask,
    /// Still typing. Wait.
    StillTyping,
    /// Nothing has been typed at all, so there is nothing to continue.
    Idle,
    /// Not enough text to continue.
    TooShort,
    /// Too much text for this to be a sentence in progress.
    TooLong,
    /// Asked too recently.
    TooSoon,
}

impl Decision {
    pub fn should_ask(&self) -> bool {
        matches!(self, Decision::Ask)
    }
}

/// Whether to ask for a continuation.
///
/// Ordered so the cheapest and most common refusals come first, and so a caller reading the answer
/// gets the *reason* rather than a boolean — "why is nothing being suggested" is the question this
/// feature generates most.
pub fn decide(
    policy: &CompletionPolicy,
    now_ms: i64,
    last_keystroke_ms: Option<i64>,
    last_asked_ms: Option<i64>,
    text: &str,
) -> Decision {
    let Some(last_keystroke_ms) = last_keystroke_ms else {
        return Decision::Idle;
    };

    if now_ms.saturating_sub(last_keystroke_ms) < policy.pause_ms {
        return Decision::StillTyping;
    }

    let length = text.chars().count();
    if length < policy.min_chars {
        return Decision::TooShort;
    }
    if length > policy.max_chars {
        return Decision::TooLong;
    }

    if let Some(asked) = last_asked_ms {
        if now_ms.saturating_sub(asked) < policy.min_gap_ms {
            return Decision::TooSoon;
        }
    }

    Decision::Ask
}

/// The exact text to insert at the caret, or nothing.
///
/// # Why this is not just "take the reply"
///
/// Asked to continue a sentence, a model does one of four things: continues it, repeats the sentence
/// and then continues, wraps the answer in quotes or a code fence, or answers *about* the sentence.
/// Only the first two are usable, and inserting either of the others at the caret puts visible
/// nonsense into somebody's document.
///
/// # Why the joining is part of the job
///
/// A continuation of "…lower than" is "expected", and inserting that verbatim gives "thanexpected".
/// Whether a space belongs in front depends on what the text ends with and what the continuation
/// starts with, and the only place that can be decided correctly is here — a caller holding a
/// string with an ambiguous leading space has already lost the information.
pub fn continuation_of(text: &str, reply: &str) -> Option<String> {
    let body = strip_wrapping(reply);
    if body.is_empty() {
        return None;
    }

    // How much of the reply is the model repeating what it was given.
    let echoed = echoed_prefix(text, body);
    let rest = body[echoed..].trim();

    if rest.is_empty() {
        // The reply was only an echo. Not a suggestion.
        return None;
    }

    // A reply that did not echo anything might not be a continuation at all.
    if echoed == 0 && reads_like_commentary(rest) {
        return None;
    }

    Some(join(text, rest))
}

/// Put a continuation onto the end of some text, with a space if one is needed and not otherwise.
///
/// Separate and public because it is the part with a right answer that a test can state.
pub fn join(text: &str, continuation: &str) -> String {
    let continuation = continuation.trim();
    if continuation.is_empty() {
        return String::new();
    }

    // Already separated: the caret is after a space or a newline.
    if text.is_empty() || text.ends_with(char::is_whitespace) {
        return continuation.to_string();
    }

    // Punctuation that attaches to the word before it. A space in front of a comma is wrong in
    // every language this could plausibly be used in.
    let attaches = continuation.chars().next().is_some_and(|c| {
        matches!(
            c,
            ',' | '.' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\'' | '’' | '”' | '%'
        )
    });

    if attaches {
        continuation.to_string()
    } else {
        format!(" {continuation}")
    }
}

/// How many bytes at the start of `reply` are the tail of `text` repeated back.
///
/// The longest such overlap, so a model that repeated the whole field is handled the same way as one
/// that repeated the last clause. Bounded, because scanning every suffix of a four-thousand
/// character field for every completion is work nobody asked for.
///
/// Overlaps shorter than a few characters are ignored: a reply beginning with the same single letter
/// the text happens to end with is a coincidence, and cutting it would delete a real character.
fn echoed_prefix(text: &str, reply: &str) -> usize {
    const MAX_OVERLAP: usize = 400;
    const MIN_OVERLAP: usize = 3;

    let text = text.trim_end();
    let floor = text.len().saturating_sub(MAX_OVERLAP);

    // `char_indices` ascends, and a later index is a shorter suffix — so the first match found is
    // the longest one.
    for (index, _) in text.char_indices() {
        if index < floor {
            continue;
        }
        let suffix = &text[index..];
        if suffix.len() < MIN_OVERLAP {
            break;
        }
        if reply.starts_with(suffix) {
            return suffix.len();
        }
    }

    0
}

/// Strip code fences and surrounding quotes, and trim.
///
/// The trim is deliberate and the joining above is what makes it safe: throwing away the reply's own
/// leading space loses nothing, because whether a space belongs there is decided from the text
/// rather than from what the model happened to emit.
fn strip_wrapping(reply: &str) -> &str {
    let mut reply = reply.trim();

    if let Some(inner) = reply.strip_prefix("```") {
        // Drop the language tag if there is one, then the closing fence.
        let inner = inner
            .split_once('\n')
            .map(|(_, rest)| rest)
            .unwrap_or(inner);
        reply = inner.trim().trim_end_matches("```").trim();
    }

    for quote in ['"', '\''] {
        if reply.len() >= 2 && reply.starts_with(quote) && reply.ends_with(quote) {
            reply = &reply[1..reply.len() - 1];
        }
    }

    reply
}

/// Whether a reply is the model talking about the text rather than continuing it.
///
/// A small, deliberately conservative list. The cost of a false positive is one suggestion not
/// shown; the cost of a false negative is "Sure! Here is the rest of your sentence:" typed into
/// somebody's email.
fn reads_like_commentary(reply: &str) -> bool {
    let lowered = reply.trim_start().to_ascii_lowercase();

    const OPENERS: &[&str] = &[
        "sure",
        "certainly",
        "here is",
        "here's",
        "here are",
        "the text",
        "the sentence",
        "it seems",
        "it looks like",
        "i think",
        "i would",
        "as an ai",
        "of course",
        "based on",
        "to continue",
        "continuing",
    ];

    OPENERS.iter().any(|opener| lowered.starts_with(opener))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    fn policy() -> CompletionPolicy {
        CompletionPolicy::default()
    }

    fn enough_text() -> &'static str {
        "The quarterly numbers came in lower than"
    }

    // ------------------------------------------------------------ when to ask

    #[test]
    fn a_pause_after_enough_text_is_when_to_ask() {
        let decision = decide(&policy(), NOW, Some(NOW - 900), None, enough_text());
        assert_eq!(decision, Decision::Ask);
        assert!(decision.should_ask());
    }

    /// A fast typist leaves eighty milliseconds between keys, and that is not a pause.
    #[test]
    fn the_gap_between_two_words_is_not_a_pause() {
        for gap in [0, 80, 200, 599] {
            assert_eq!(
                decide(&policy(), NOW, Some(NOW - gap), None, enough_text()),
                Decision::StillTyping,
                "{gap}ms should read as still typing"
            );
        }
    }

    /// Completing an empty field is guessing what somebody is about to write.
    #[test]
    fn there_is_nothing_to_continue_before_anything_is_typed() {
        assert_eq!(decide(&policy(), NOW, None, None, ""), Decision::Idle);
        assert_eq!(
            decide(&policy(), NOW, Some(NOW - 5_000), None, "hi"),
            Decision::TooShort
        );
    }

    #[test]
    fn a_document_is_not_a_sentence_in_progress() {
        let huge = "word ".repeat(2_000);
        assert_eq!(
            decide(&policy(), NOW, Some(NOW - 5_000), None, &huge),
            Decision::TooLong
        );
    }

    /// The rate limit. Without it a slow model and a hesitant typist queue up stale answers.
    #[test]
    fn asking_twice_in_quick_succession_is_refused() {
        assert_eq!(
            decide(
                &policy(),
                NOW,
                Some(NOW - 900),
                Some(NOW - 500),
                enough_text()
            ),
            Decision::TooSoon
        );
        assert_eq!(
            decide(
                &policy(),
                NOW,
                Some(NOW - 900),
                Some(NOW - 3_000),
                enough_text()
            ),
            Decision::Ask
        );
    }

    /// "Why is nothing being suggested" is the question this feature generates most, so every
    /// refusal is a distinct answer rather than a false.
    #[test]
    fn every_refusal_says_which_one_it_is() {
        let reasons = [
            decide(&policy(), NOW, None, None, ""),
            decide(&policy(), NOW, Some(NOW), None, enough_text()),
            decide(&policy(), NOW, Some(NOW - 5_000), None, "no"),
            decide(&policy(), NOW, Some(NOW - 5_000), None, &"x".repeat(9_000)),
            decide(&policy(), NOW, Some(NOW - 900), Some(NOW), enough_text()),
        ];

        let distinct: std::collections::BTreeSet<String> =
            reasons.iter().map(|r| format!("{r:?}")).collect();
        assert_eq!(distinct.len(), 5, "{reasons:?}");
        assert!(!reasons.iter().any(Decision::should_ask));
    }

    /// Clocks go backwards over an NTP correction, and that must not become a panic.
    #[test]
    fn a_timestamp_from_the_future_does_not_overflow() {
        assert_eq!(
            decide(&policy(), NOW, Some(NOW + 10_000), None, enough_text()),
            Decision::StillTyping
        );
    }

    // ------------------------------------------------------------ what counts as a continuation

    #[test]
    fn a_plain_continuation_is_taken_as_it_is() {
        assert_eq!(
            continuation_of(
                enough_text(),
                " expected, largely because of the delayed launch."
            ),
            Some(" expected, largely because of the delayed launch.".to_string())
        );
    }

    /// Models repeat what they were given. Very common, and recoverable.
    #[test]
    fn a_repeated_prefix_is_stripped() {
        let reply = format!("{} expected.", enough_text());
        assert_eq!(
            continuation_of(enough_text(), &reply),
            Some(" expected.".to_string())
        );
    }

    /// And one that repeated only the last clause is handled the same way.
    #[test]
    fn a_repeated_tail_is_stripped() {
        let reply = "numbers came in lower than expected, by about four percent.";
        assert_eq!(
            continuation_of(enough_text(), reply),
            Some(" expected, by about four percent.".to_string())
        );
    }

    /// A single coincidental character is not an echo, and cutting it would delete a real letter.
    #[test]
    fn a_one_character_coincidence_is_not_treated_as_an_echo() {
        // The text ends in "n" and the continuation starts with "n".
        assert_eq!(
            continuation_of("we should not run", "now, before the release"),
            Some(" now, before the release".to_string())
        );
    }

    #[test]
    fn a_code_fence_is_removed() {
        assert_eq!(
            continuation_of(enough_text(), "```\n expected.\n```"),
            Some(" expected.".to_string())
        );
        assert_eq!(
            continuation_of(enough_text(), "```text\n expected.\n```"),
            Some(" expected.".to_string())
        );
    }

    #[test]
    fn surrounding_quotes_are_removed() {
        assert_eq!(
            continuation_of(enough_text(), "\" expected.\""),
            Some(" expected.".to_string())
        );
    }

    // ------------------------------------------------------------ joining

    /// "thanexpected" is the bug this exists to prevent.
    #[test]
    fn a_space_is_added_when_one_is_needed() {
        assert_eq!(join("lower than", "expected"), " expected");
    }

    #[test]
    fn a_space_is_not_added_when_the_caret_is_already_after_one() {
        assert_eq!(join("lower than ", "expected"), "expected");
        assert_eq!(join("a line\n", "expected"), "expected");
    }

    /// A space in front of a comma is wrong in every language this could plausibly be used in.
    #[test]
    fn punctuation_attaches_to_the_word_before_it() {
        for continuation in [",", ".", "; and then", "!", "?", "')", "% of revenue"] {
            let joined = join("lower than expected", continuation);
            assert!(
                !joined.starts_with(' '),
                "{continuation:?} should attach, got {joined:?}"
            );
        }
    }

    #[test]
    fn joining_nothing_produces_nothing() {
        assert_eq!(join("lower than", "   "), "");
        assert_eq!(join("", "expected"), "expected");
    }

    /// The failure this guard exists for: "Sure! Here is the rest" typed into somebody's email.
    #[test]
    fn commentary_is_refused_rather_than_inserted() {
        for reply in [
            "Sure! Here is a continuation: expected.",
            "Here's how I would finish that sentence.",
            "It looks like you are writing about revenue.",
            "I think you mean the quarterly report.",
            "As an AI language model, I cannot know your numbers.",
            "Based on the context, the sentence could end with 'expected'.",
            "To continue: expected.",
        ] {
            assert_eq!(continuation_of(enough_text(), reply), None, "{reply}");
        }
    }

    #[test]
    fn an_empty_reply_suggests_nothing() {
        assert_eq!(continuation_of(enough_text(), ""), None);
        assert_eq!(continuation_of(enough_text(), "   \n  "), None);
        assert_eq!(continuation_of(enough_text(), "```\n\n```"), None);
    }

    /// A reply that is only the text back again is not a suggestion.
    #[test]
    fn an_exact_echo_suggests_nothing() {
        assert_eq!(continuation_of(enough_text(), enough_text()), None);
    }

    /// The policy is serialised into settings, so its shape is worth pinning.
    #[test]
    fn a_policy_round_trips_through_json() {
        let policy = policy();
        let json = serde_json::to_string(&policy).expect("serializes");
        assert_eq!(
            serde_json::from_str::<CompletionPolicy>(&json).expect("deserializes"),
            policy
        );
    }

    /// The defaults have to be a pause a person would notice as one, and a rate a local model can
    /// keep up with.
    #[test]
    fn the_defaults_are_a_real_pause_and_a_real_rate_limit() {
        let policy = policy();
        assert!(policy.pause_ms >= 400 && policy.pause_ms <= 1_200);
        assert!(policy.min_gap_ms >= policy.pause_ms);
        assert!(policy.min_chars > 0);
        assert!(policy.max_chars > policy.min_chars);
    }
}
