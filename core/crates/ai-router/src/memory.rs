//! Deciding what is worth remembering.
//!
//! # Two passes, and only one of them calls a model
//!
//! The **observer** reads a transcript and proposes candidate facts. That is a model call, and it is
//! the easy half.
//!
//! The **reflector** decides which candidates survive: rejecting duplicates, rejecting claims about
//! other people, and choosing a scope. It is a pure function over candidates and existing memories,
//! so every rule in it is testable exhaustively with no model — which matters because this is where
//! the decisions that can be wrong live. It is the same split [`crate::clarify`] uses: the model call
//! in one place, the judgement about whether to act in another.
//!
//! # Why third-party facts are rejected rather than scoped
//!
//! A memory is injected into the system prompt of future calls. A sentence like "Dana is difficult in
//! reviews" would then colour every answer, indefinitely, about a person who never consented to being
//! characterised and cannot see or correct it. There is no scope narrow enough to make that
//! acceptable, so it is not stored at all.
//!
//! The rejection here is a heuristic over a model's output, not a guarantee. It is one of three
//! defences: the prompt asks only for facts about the user, this filters what comes back, and every
//! stored memory is visible and deletable. None is sufficient alone.

use serde::{Deserialize, Serialize};

/// A fact the observer thinks is worth keeping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub text: String,
    /// Whether the observer judged this to be about the whole workspace rather than one project.
    #[serde(default)]
    pub global: bool,
}

/// What the reflector decided about one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Store it.
    Keep { global: bool },
    /// An existing memory already says this.
    Duplicate { existing: String },
    /// A claim about somebody other than the user.
    ThirdParty { reason: String },
    /// Too vague, too long, or empty.
    Unusable { reason: String },
}

impl Verdict {
    pub fn kept(&self) -> bool {
        matches!(self, Verdict::Keep { .. })
    }
}

/// The longest a memory may be.
///
/// A memory is a fact, not a paragraph. Something that needs 300 characters is a note, and it would
/// consume prompt budget that belongs to the actual transcript.
pub const MAX_LEN: usize = 200;

/// The shortest useful memory.
///
/// Below this it cannot carry a fact — "yes", "ok", "the team".
pub const MIN_LEN: usize = 8;

/// Pronouns and phrasings that make a sentence about the user.
const FIRST_PERSON: &[&str] = &[
    "i ", "i'm", "i am", "i'd", "i've", "i'll", "my ", "me ", "mine", "we ", "our ", "us ",
];

/// Words that make a sentence a characterisation of somebody else.
///
/// Deliberately blunt. A false positive costs one memory that could have been rephrased; a false
/// negative puts a judgement about a real person into every future prompt.
const THIRD_PERSON: &[&str] = &[
    " he ", " she ", " they ", " him ", " her ", " them ", " his ", " hers ", " their ",
];

/// Decide what to do with one candidate, given what is already remembered.
///
/// `existing` is every memory currently applicable, as plain text.
pub fn reflect(candidate: &Candidate, existing: &[String]) -> Verdict {
    let text = candidate.text.trim();
    let lower = format!(" {} ", text.to_lowercase());

    if text.len() < MIN_LEN {
        return Verdict::Unusable {
            reason: "too short to carry a fact".into(),
        };
    }
    if text.len() > MAX_LEN {
        return Verdict::Unusable {
            reason: format!("longer than {MAX_LEN} characters; that is a note, not a memory"),
        };
    }

    // Checked before duplication: a third-party claim that happens to duplicate an existing one
    // should still be reported as the thing that makes it unacceptable.
    let is_first_person = FIRST_PERSON.iter().any(|p| lower.contains(p));
    let names_someone_else = THIRD_PERSON.iter().any(|p| lower.contains(p));

    if !is_first_person {
        // Either way this is not stored, and that is the property that matters. The two verdicts
        // differ only in what the UI can say.
        //
        // A pronoun is reliable evidence of a third party. A *name* is not detectable here: I tried
        // capitalisation and it missed "Dana is difficult" — the name is sentence-initial — while
        // flagging "Fridays" as a person. A heuristic that cannot tell a weekday from a colleague is
        // worse than none, so a named person without a pronoun is reported as the honest thing: not
        // a fact about the user.
        if names_someone_else {
            return Verdict::ThirdParty {
                reason: "describes somebody other than the user".into(),
            };
        }
        // Not about the user and not about anyone else either — an observation about the world,
        // which is what the transcript is for.
        return Verdict::Unusable {
            reason: "not a fact about the user".into(),
        };
    }

    if let Some(existing) = existing.iter().find(|e| says_the_same(e, text)) {
        return Verdict::Duplicate {
            existing: existing.clone(),
        };
    }

    Verdict::Keep {
        global: candidate.global,
    }
}

/// Whether two memories say the same thing.
///
/// Word-overlap rather than embeddings: this runs during a background pass that may have no embedder,
/// and "I prefer short summaries" versus "I prefer summaries that are short" is the shape of
/// duplicate that actually shows up. It is deliberately generous — keeping two phrasings of one fact
/// wastes a capped slot, which is worse than dropping a near-duplicate.
fn says_the_same(a: &str, b: &str) -> bool {
    let words = |s: &str| -> std::collections::BTreeSet<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 3)
            .map(str::to_string)
            .collect()
    };

    let (a, b) = (words(a), words(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }

    let shared = a.intersection(&b).count();
    let smaller = a.len().min(b.len());
    shared * 100 >= smaller * 70
}

/// Run the reflector over a batch, stopping when the remaining room runs out.
///
/// `room` is how many more memories the target scope can hold. Returned verdicts cover every
/// candidate regardless, so a caller can report what was skipped and why.
pub fn reflect_batch(
    candidates: &[Candidate],
    existing: &[String],
    room: usize,
) -> Vec<(Candidate, Verdict)> {
    let mut seen: Vec<String> = existing.to_vec();
    let mut kept = 0;
    let mut out = Vec::with_capacity(candidates.len());

    for candidate in candidates {
        let verdict = if kept >= room {
            Verdict::Unusable {
                reason: "no room left in this scope".into(),
            }
        } else {
            reflect(candidate, &seen)
        };

        if verdict.kept() {
            kept += 1;
            // Added so two candidates in one batch that say the same thing do not both survive.
            seen.push(candidate.text.trim().to_string());
        }
        out.push((candidate.clone(), verdict));
    }

    out
}

/// The system-prompt section memories are injected as.
///
/// Empty when there is nothing to say, so a prompt does not carry a heading with nothing under it.
pub fn as_prompt_section(memories: &[String]) -> String {
    if memories.is_empty() {
        return String::new();
    }

    let mut out = String::from("Things to keep in mind about the person you are helping:\n");
    for memory in memories {
        out.push_str("- ");
        out.push_str(memory.trim());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(text: &str) -> Candidate {
        Candidate {
            text: text.into(),
            global: false,
        }
    }

    #[test]
    fn a_first_person_preference_is_kept() {
        let v = reflect(&candidate("I prefer short summaries with bullets"), &[]);
        assert_eq!(v, Verdict::Keep { global: false });
    }

    /// The rule that matters most. A characterisation of a colleague would colour every future
    /// answer about somebody who never consented and cannot see it.
    #[test]
    fn a_claim_about_somebody_else_is_rejected() {
        for text in [
            "He always misses deadlines",
            "Their estimates are never accurate",
            "She prefers async updates",
        ] {
            assert!(
                matches!(reflect(&candidate(text), &[]), Verdict::ThirdParty { .. }),
                "{text:?} should be rejected as a third-party claim"
            );
        }
    }

    /// A characterisation that names somebody without a pronoun. What matters is that it is not
    /// stored; the reason given is the weaker "not about the user", because a name cannot be
    /// detected here reliably — see the comment in `reflect`.
    #[test]
    fn a_named_characterisation_is_still_never_stored() {
        let v = reflect(&candidate("Dana is difficult in design reviews"), &[]);
        assert!(!v.kept(), "{v:?}");
    }

    /// A sentence about the user that mentions someone else is still about the user.
    #[test]
    fn a_first_person_fact_that_mentions_others_is_still_kept() {
        let v = reflect(
            &candidate("I run the platform standup with them weekly"),
            &[],
        );
        assert_eq!(v, Verdict::Keep { global: false });
    }

    #[test]
    fn something_that_is_about_nobody_is_unusable() {
        let v = reflect(&candidate("The deploy happens on Fridays"), &[]);
        assert!(matches!(v, Verdict::Unusable { .. }), "{v:?}");
    }

    #[test]
    fn too_short_and_too_long_are_both_refused() {
        assert!(matches!(
            reflect(&candidate("I am"), &[]),
            Verdict::Unusable { .. }
        ));

        let long = format!("I prefer {}", "very ".repeat(60));
        assert!(matches!(
            reflect(&candidate(&long), &[]),
            Verdict::Unusable { .. }
        ));
    }

    #[test]
    fn a_rephrasing_of_something_already_remembered_is_a_duplicate() {
        let existing = vec!["I prefer short summaries with bullets".to_string()];
        let v = reflect(
            &candidate("I prefer summaries that are short, with bullets"),
            &existing,
        );
        assert!(matches!(v, Verdict::Duplicate { .. }), "{v:?}");
    }

    #[test]
    fn an_unrelated_fact_is_not_a_duplicate() {
        let existing = vec!["I prefer short summaries with bullets".to_string()];
        let v = reflect(
            &candidate("I work in the Europe/London timezone"),
            &existing,
        );
        assert_eq!(v, Verdict::Keep { global: false });
    }

    /// Two candidates in one batch saying the same thing must not both survive and burn two of a
    /// capped five slots.
    #[test]
    fn a_batch_deduplicates_within_itself() {
        let candidates = vec![
            candidate("I prefer short summaries with bullets"),
            candidate("I prefer summaries that are short, with bullets"),
        ];
        let verdicts = reflect_batch(&candidates, &[], 5);

        assert!(verdicts[0].1.kept());
        assert!(
            matches!(verdicts[1].1, Verdict::Duplicate { .. }),
            "{:?}",
            verdicts[1].1
        );
    }

    #[test]
    fn a_batch_stops_when_the_scope_is_full() {
        let candidates = vec![
            candidate("I prefer short summaries with bullets"),
            candidate("I work in the Europe/London timezone"),
        ];
        let verdicts = reflect_batch(&candidates, &[], 1);

        assert!(verdicts[0].1.kept());
        assert!(!verdicts[1].1.kept(), "the cap has to bind mid-batch");
    }

    #[test]
    fn with_no_room_nothing_is_kept_but_everything_is_reported() {
        let candidates = vec![candidate("I prefer short summaries with bullets")];
        let verdicts = reflect_batch(&candidates, &[], 0);

        assert_eq!(
            verdicts.len(),
            1,
            "a caller has to be able to say what was skipped"
        );
        assert!(!verdicts[0].1.kept());
    }

    #[test]
    fn the_prompt_section_is_empty_when_there_is_nothing_to_say() {
        assert_eq!(as_prompt_section(&[]), "");
    }

    #[test]
    fn the_prompt_section_lists_what_is_remembered() {
        let section = as_prompt_section(&[
            "I prefer short summaries".to_string(),
            "I work in Europe/London".to_string(),
        ]);
        assert!(section.contains("- I prefer short summaries"));
        assert!(section.contains("- I work in Europe/London"));
    }
}
