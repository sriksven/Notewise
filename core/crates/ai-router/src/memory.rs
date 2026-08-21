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
//!
//! # Why a secret is refused rather than redacted
//!
//! A memory goes into the system prompt of every future call it applies to. Storing "my key is
//! sk-abc" and relying on redaction to mask it each time is one missed code path away from leaking
//! it forever; refusing to store it has no such failure mode. So [`reflect`] rejects anything the
//! redactor recognises, at write time.

use serde::{Deserialize, Serialize};

use crate::redact::{redact, Category, RedactionPolicy};

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
    /// Something that should not be written down at all — a key, a card number, a phone number.
    Secret { category: Category },
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

    // Before anything else about who it is about. A key is not storable whether or not it belongs to
    // the user, and the strictest policy is used deliberately: this is deciding what to *keep
    // forever*, not what to send once, so a contact detail counts too.
    if let Some(category) = contains_secret(text) {
        return Verdict::Secret { category };
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

/// The first thing in this text the redactor recognises, if any.
///
/// Uses [`RedactionPolicy::SecretsAndContacts`] — the strictest — rather than whatever policy the
/// router happens to be on. The question here is not "what should be masked on the way out" but
/// "what should never be written down", and a phone number in a durable fact injected into every
/// future prompt is in the second category even when it would have been fine in the first.
pub fn contains_secret(text: &str) -> Option<Category> {
    let (_, report) = redact(text, RedactionPolicy::SecretsAndContacts);
    report.counts().first().map(|(category, _)| *category)
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

/// How many candidates one run may propose.
///
/// Three, because the caps are five and twenty: a run that proposed ten would fill the global scope
/// from one meeting and leave nothing for the next month of them. It also bounds the damage from a
/// model that has decided everything is worth remembering.
pub const MAX_CANDIDATES: usize = 3;

/// The instruction block for the observer.
///
/// Written as one string so the whole contract is visible at once, and stated in the negative more
/// than the positive — the failure that matters is not "it missed a fact" but "it wrote down
/// something about a colleague", and a prompt guards against that by forbidding it explicitly rather
/// than by describing what is wanted and hoping.
pub fn observer_prompt() -> String {
    format!(
        "You read a person's meeting transcripts and note durable facts **about that person** which \
would help you help them in future.

Reply with EXACTLY ONE JSON object and nothing else:

{{\"memories\": [{{\"text\": \"<one short fact>\", \"global\": true}}]}}

Return at most {MAX_CANDIDATES}. Return `{{\"memories\": []}}` if there is nothing worth keeping — \
that is the common and correct answer for most meetings.

What to record:
- Their role, what they are responsible for, what they work on.
- Vocabulary their team uses that you would otherwise misread.
- How they like things done — the format of a summary, the tone of a message.
- Long-running projects and what they are for.

What NOT to record, ever:
- Anything about another person. Not their role, not their performance, not their plans, not \
their health, not who they report to. If a fact is about somebody who is not the person you are \
helping, leave it out. There is no version of it that is acceptable.
- Anything that happened once. A decision, a date, an action item — those live in the meeting.
- Secrets. Keys, tokens, card numbers, phone numbers, addresses.
- Anything you inferred rather than heard.

Set `global` to true when the fact is true everywhere, and false when it only applies to the \
project this meeting belongs to.

Write each fact in the person's own voice, starting with \"I\" or \"my\" — \"I own the billing \
service\", not \"the user owns the billing service\"."
    )
}

/// Read candidates out of whatever the model actually said.
///
/// Tolerant of code fences and surrounding prose, like every other JSON protocol in this codebase:
/// a model asked for JSON wraps it. An unreadable reply yields nothing, which the caller treats as
/// "this meeting had nothing worth keeping" — the same outcome as an honest empty answer, and the
/// safe direction for a feature whose failure mode is remembering too much.
pub fn parse_candidates(reply: &str) -> Vec<Candidate> {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        memories: Vec<Candidate>,
    }

    let Some(object) = first_object(reply) else {
        return Vec::new();
    };

    let mut candidates = match serde_json::from_str::<Envelope>(&object) {
        Ok(envelope) => envelope.memories,
        // A bare array, which models produce about a third of the time.
        Err(_) => serde_json::from_str::<Vec<Candidate>>(&object).unwrap_or_default(),
    };

    // Truncated rather than rejected: a model that returned six useful facts should not cost the
    // user all six, and the cap is about prompt budget rather than about correctness.
    candidates.truncate(MAX_CANDIDATES);
    candidates
}

/// The first balanced JSON object or array in a string.
fn first_object(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.iter().position(|c| *c == '{' || *c == '[')?;
    let (open, close) = if chars[start] == '{' {
        ('{', '}')
    } else {
        ('[', ']')
    };

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in chars[start..].iter().enumerate() {
        if in_string {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match *ch {
            '"' => in_string = true,
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(chars[start..=start + offset].iter().collect());
                }
            }
            _ => {}
        }
    }
    None
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
    // ------------------------------------------------------------ secrets, refused at write time

    /// A memory goes into every future prompt it applies to. Relying on redaction to mask it each
    /// time is one missed code path away from leaking it forever.
    #[test]
    fn a_candidate_carrying_a_secret_is_refused_rather_than_stored() {
        let cases = [
            "my api key is sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "I pay for it with 4111 1111 1111 1111",
            "my number is +1 415 555 0132",
            "I use https://admin:hunter2@internal.example.com every morning",
        ];

        for text in cases {
            match reflect(&candidate(text), &[]) {
                Verdict::Secret { .. } => {}
                other => panic!("{text:?} should have been refused, got {other:?}"),
            }
        }
    }

    /// Checked before who it is about: a key is not storable whether or not it belongs to the user.
    #[test]
    fn a_secret_is_refused_even_when_the_sentence_is_about_the_user() {
        let text = "my card is 4111 1111 1111 1111 and I use it for the team subscription";
        assert!(matches!(
            reflect(&candidate(text), &[]),
            Verdict::Secret { .. }
        ));
    }

    /// An ordinary fact must not be refused for looking vaguely numeric.
    #[test]
    fn an_ordinary_fact_is_not_mistaken_for_a_secret() {
        for text in [
            "I own the billing service and the 3 workers behind it",
            "I run the platform standup at 9am on Mondays",
            "my team calls the ingest pipeline the funnel",
        ] {
            assert!(
                !matches!(reflect(&candidate(text), &[]), Verdict::Secret { .. }),
                "{text:?} was refused as a secret"
            );
            assert_eq!(contains_secret(text), None, "{text:?}");
        }
    }

    // ------------------------------------------------------------ the observer's output

    #[test]
    fn candidates_are_read_out_of_a_bare_object() {
        let parsed = parse_candidates(
            r#"{"memories":[{"text":"I own the billing service","global":true}]}"#,
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].text, "I own the billing service");
        assert!(parsed[0].global);
    }

    /// A model asked for JSON wraps it.
    #[test]
    fn a_fenced_reply_is_read() {
        let parsed = parse_candidates(
            "Here is what I found:\n```json\n{\"memories\": [{\"text\": \"I prefer short summaries\"}]}\n```",
        );
        assert_eq!(parsed.len(), 1);
        assert!(!parsed[0].global, "global defaults to false");
    }

    /// Models produce a bare array about a third of the time.
    #[test]
    fn a_bare_array_is_read() {
        let parsed = parse_candidates(r#"[{"text":"I own the billing service"}]"#);
        assert_eq!(parsed.len(), 1);
    }

    /// The common and correct answer for most meetings.
    #[test]
    fn an_empty_answer_is_read_as_nothing_to_keep() {
        assert!(parse_candidates(r#"{"memories": []}"#).is_empty());
        assert!(parse_candidates("Nothing stood out.").is_empty());
        assert!(parse_candidates("").is_empty());
    }

    /// The safe direction for a feature whose failure mode is remembering too much.
    #[test]
    fn an_unreadable_reply_yields_nothing_rather_than_guessing() {
        assert!(parse_candidates(r#"{"memories": "I own billing"}"#).is_empty());
        assert!(parse_candidates("{ this is not json }").is_empty());
        assert!(parse_candidates(r#"{"memories":[{"text":"#).is_empty());
    }

    /// A run that proposed ten would fill the global scope from one meeting.
    #[test]
    fn more_candidates_than_the_cap_are_truncated_rather_than_rejected() {
        let many: String = (0..8)
            .map(|n| format!(r#"{{"text":"I do thing number {n} every week"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let parsed = parse_candidates(&format!(r#"{{"memories":[{many}]}}"#));

        assert_eq!(parsed.len(), MAX_CANDIDATES);
        assert!(
            parsed[0].text.contains("number 0"),
            "the first ones are kept"
        );
    }

    /// A brace inside a memory must not truncate the object.
    #[test]
    fn a_brace_inside_a_memory_does_not_end_the_object() {
        let parsed =
            parse_candidates(r#"{"memories":[{"text":"I write my notes as {topic}: detail"}]}"#);
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].text.contains("{topic}"), "{:?}", parsed[0].text);
    }

    // ------------------------------------------------------------ the prompt

    /// The failure that matters is not a missed fact but a fact about a colleague, so the prompt
    /// forbids it explicitly rather than describing what is wanted and hoping.
    #[test]
    fn the_prompt_forbids_third_party_facts_in_so_many_words() {
        let prompt = observer_prompt();
        assert!(prompt.contains("about another person"), "{prompt}");
        assert!(
            prompt.contains("no version of it that is acceptable"),
            "{prompt}"
        );
        assert!(prompt.contains("Secrets"), "{prompt}");
        assert!(
            prompt.contains(&MAX_CANDIDATES.to_string()),
            "the prompt must state the limit it is held to"
        );
    }

    /// Asking for the user's own voice is what makes the reflector's first-person check work at all.
    #[test]
    fn the_prompt_asks_for_the_first_person() {
        let prompt = observer_prompt();
        assert!(prompt.contains("own voice"), "{prompt}");
        assert!(prompt.contains("I own the billing service"), "{prompt}");
    }

    /// "Nothing worth keeping" has to be presented as normal, or a model invents something.
    #[test]
    fn the_prompt_makes_an_empty_answer_the_expected_one() {
        assert!(observer_prompt().contains("common and correct answer"));
    }
}
