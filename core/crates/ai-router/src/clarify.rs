//! Real-time clarifying questions.
//!
//! Watches a meeting as it happens and surfaces questions worth asking *while the people who
//! can answer them are still in the room*. A summary telling you afterwards that a decision
//! had no owner is far less useful than a nudge at the moment it was made.
//!
//! # Why the orchestration matters more than the prompt
//!
//! Getting a model to spot vagueness is easy. Making it useful during a live meeting is not,
//! and almost all of the difficulty is in *when* to speak rather than *what* to say:
//!
//! - **Interrupting badly is worse than staying silent.** A suggestion arriving every thirty
//!   seconds gets the panel closed within one meeting, and then the feature does not exist.
//!   [`ClarifierConfig::cooldown_ms`] enforces a floor between suggestions.
//! - **Stale questions are noise.** Asking about something said fifteen minutes ago derails
//!   the conversation. Questions expire ([`ClarifierConfig::staleness_ms`]).
//! - **Repeats destroy trust fastest.** The same ambiguity re-surfacing makes the whole
//!   feature feel broken, so questions are deduplicated on normalized text.
//!
//! The model call lives in [`suggest_questions`]; the decision of whether to make it at all
//! lives in [`ClarifierSession`], which is pure and therefore testable without a model.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::types::{ChatMessage, ChatRequest};
use crate::AiBackend;

/// Why a statement needs clarifying.
///
/// Categories rather than free text so the UI can group, filter, and let a user turn off the
/// kinds they find unhelpful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmbiguityKind {
    /// "that approach", "the other thing" — a referent nobody stated.
    VagueReference,
    /// "much faster", "soon", "a lot" — a claim with no number attached.
    Unquantified,
    /// "someone should", "we'll get to it" — work with no owner.
    UnassignedAction,
    /// A commitment with no date.
    MissingDeadline,
    /// An acronym or term used without being defined.
    UndefinedTerm,
    /// Conflicts with something said earlier in the meeting.
    Contradiction,
    /// A decision recorded without the reasoning behind it.
    UnstatedRationale,
}

impl AmbiguityKind {
    pub const ALL: &'static [AmbiguityKind] = &[
        AmbiguityKind::VagueReference,
        AmbiguityKind::Unquantified,
        AmbiguityKind::UnassignedAction,
        AmbiguityKind::MissingDeadline,
        AmbiguityKind::UndefinedTerm,
        AmbiguityKind::Contradiction,
        AmbiguityKind::UnstatedRationale,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AmbiguityKind::VagueReference => "vague_reference",
            AmbiguityKind::Unquantified => "unquantified",
            AmbiguityKind::UnassignedAction => "unassigned_action",
            AmbiguityKind::MissingDeadline => "missing_deadline",
            AmbiguityKind::UndefinedTerm => "undefined_term",
            AmbiguityKind::Contradiction => "contradiction",
            AmbiguityKind::UnstatedRationale => "unstated_rationale",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s)
    }

    /// How much this kind costs if it goes unasked.
    ///
    /// A contradiction nobody catches becomes two people building different things; a missing
    /// definition is usually recoverable later. Used to rank when several are pending.
    pub fn weight(&self) -> u8 {
        match self {
            AmbiguityKind::Contradiction => 5,
            AmbiguityKind::UnassignedAction => 4,
            AmbiguityKind::MissingDeadline => 3,
            AmbiguityKind::VagueReference => 3,
            AmbiguityKind::UnstatedRationale => 2,
            AmbiguityKind::Unquantified => 2,
            AmbiguityKind::UndefinedTerm => 1,
        }
    }
}

/// A question the user could ask right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarifyingQuestion {
    /// Phrased so it can be read aloud verbatim.
    pub question: String,
    /// The transcript text that prompted it, so the user can see what it refers to.
    pub about: String,
    pub kind: AmbiguityKind,
    /// When the prompting statement was said, for staleness.
    pub at_ms: i64,
}

impl ClarifyingQuestion {
    /// Key used for deduplication.
    ///
    /// Normalized so trivial rewordings of the same question collapse together — the model
    /// will not phrase an ambiguity identically twice, and a near-duplicate is just as
    /// annoying as an exact one.
    pub fn dedupe_key(&self) -> String {
        let normalized: String = self
            .question
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect();

        // Content words only: "what is the timeline for the migration" and "what's the
        // migration timeline" should collide.
        let mut words: Vec<&str> = normalized
            .split_whitespace()
            .filter(|w| w.len() > 3 && !STOPWORDS.contains(w))
            .collect();
        words.sort_unstable();
        words.dedup();
        words.join(" ")
    }

    pub fn is_stale(&self, now_ms: i64, staleness_ms: i64) -> bool {
        now_ms - self.at_ms > staleness_ms
    }
}

/// Words carrying no distinguishing meaning in a question.
///
/// Contracted forms are listed explicitly: apostrophes are stripped during normalization, so
/// "what's" arrives here as "whats" and would otherwise survive as a content word — which
/// makes "what's the timeline" and "what is the timeline" look like different questions.
const STOPWORDS: &[&str] = &[
    "what", "when", "which", "that", "this", "with", "from", "have", "does", "will", "your",
    "about", "there", "would", "could", "should", "were", "been", "they", "them", "than",
    // Contractions, post-apostrophe-stripping.
    "whats", "wheres", "whens", "hows", "thats", "theres", "weve", "youre", "theyre", "isnt",
    "arent", "dont", "doesnt", "wont", "cant", "well", "were",
];

/// Tuning for a live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarifierConfig {
    /// Minimum gap between suggestions.
    ///
    /// 90s is deliberately conservative. The failure mode that kills this feature is
    /// pestering; a user who closes the panel gets nothing from it for the rest of the
    /// meeting, so silence is the safer default.
    pub cooldown_ms: i64,
    /// How much recent transcript the model sees.
    ///
    /// Enough for context, short enough that it focuses on what is being discussed now.
    pub window_ms: i64,
    /// After this long, a question is dropped unasked.
    pub staleness_ms: i64,
    /// Cap on questions held at once, highest-weight kept.
    pub max_pending: usize,
    /// Minimum transcript in the window before asking at all.
    ///
    /// Prevents firing on the first sentence of a meeting, when there is no context and
    /// everything looks ambiguous.
    pub min_window_chars: usize,
}

impl Default for ClarifierConfig {
    fn default() -> Self {
        Self {
            cooldown_ms: 90_000,
            window_ms: 120_000,
            staleness_ms: 300_000,
            max_pending: 3,
            min_window_chars: 200,
        }
    }
}

/// One transcript line, as the clarifier sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Utterance {
    pub speaker: Option<String>,
    pub text: String,
    pub at_ms: i64,
}

impl Utterance {
    pub fn new(text: impl Into<String>, at_ms: i64) -> Self {
        Self {
            speaker: None,
            text: text.into(),
            at_ms,
        }
    }

    pub fn by(mut self, speaker: impl Into<String>) -> Self {
        self.speaker = Some(speaker.into());
        self
    }
}

/// Live state for one meeting.
///
/// Pure — no model, no clock, no I/O. Time is passed in, which is what makes the interruption
/// policy testable at all: the interesting cases are about elapsed time, and waiting ninety
/// seconds in a test to check a cooldown is not a test anyone runs.
#[derive(Debug, Clone)]
pub struct ClarifierSession {
    config: ClarifierConfig,
    pending: Vec<ClarifyingQuestion>,
    asked: Vec<String>,
    last_suggestion_ms: Option<i64>,
}

impl ClarifierSession {
    pub fn new(config: ClarifierConfig) -> Self {
        Self {
            config,
            pending: Vec::new(),
            asked: Vec::new(),
            last_suggestion_ms: None,
        }
    }

    pub fn config(&self) -> ClarifierConfig {
        self.config
    }

    /// Utterances inside the current window.
    pub fn window<'a>(&self, transcript: &'a [Utterance], now_ms: i64) -> &'a [Utterance] {
        let cutoff = now_ms - self.config.window_ms;
        let start = transcript
            .iter()
            .position(|u| u.at_ms >= cutoff)
            .unwrap_or(transcript.len());
        &transcript[start..]
    }

    /// Whether to spend a model call right now.
    ///
    /// Checked before the call, not after: a suggestion that would be discarded for cooldown
    /// should not have cost a round trip and a user's tokens.
    pub fn should_suggest(&self, transcript: &[Utterance], now_ms: i64) -> bool {
        let window = self.window(transcript, now_ms);

        let chars: usize = window.iter().map(|u| u.text.len()).sum();
        if chars < self.config.min_window_chars {
            return false;
        }

        match self.last_suggestion_ms {
            None => true,
            Some(last) => now_ms - last >= self.config.cooldown_ms,
        }
    }

    /// Render the window as a prompt fragment.
    pub fn window_text(&self, transcript: &[Utterance], now_ms: i64) -> String {
        self.window(transcript, now_ms)
            .iter()
            .map(|u| match &u.speaker {
                Some(speaker) => format!("{speaker}: {}", u.text),
                None => u.text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Take questions from the model, dropping repeats and keeping the most costly.
    ///
    /// Returns the questions newly accepted, so a caller can surface only those rather than
    /// re-rendering the whole pending list.
    pub fn accept(
        &mut self,
        questions: Vec<ClarifyingQuestion>,
        now_ms: i64,
    ) -> Vec<ClarifyingQuestion> {
        self.last_suggestion_ms = Some(now_ms);

        let mut accepted = Vec::new();
        for question in questions {
            let key = question.dedupe_key();
            if key.is_empty() || self.asked.contains(&key) {
                continue;
            }
            self.asked.push(key);
            accepted.push(question);
        }

        self.pending.extend(accepted.clone());
        self.prune(now_ms);
        accepted
    }

    /// Drop stale questions and trim to the cap, keeping the highest-weight.
    pub fn prune(&mut self, now_ms: i64) {
        let staleness = self.config.staleness_ms;
        self.pending.retain(|q| !q.is_stale(now_ms, staleness));

        if self.pending.len() > self.config.max_pending {
            // Descending by weight. The sort is stable, so questions of equal weight keep
            // insertion order and the oldest survives truncation — a question raised earlier
            // has had longer to matter.
            self.pending
                .sort_by_key(|q| std::cmp::Reverse(q.kind.weight()));
            self.pending.truncate(self.config.max_pending);
        }
    }

    /// Questions currently worth showing.
    pub fn pending(&self) -> &[ClarifyingQuestion] {
        &self.pending
    }

    /// Mark a question resolved — asked, or dismissed by the user.
    pub fn resolve(&mut self, question: &ClarifyingQuestion) {
        self.pending.retain(|q| q != question);
    }

    /// Clear everything pending, e.g. when the topic visibly moves on.
    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

const SYSTEM_PROMPT: &str = "\
You are listening to a live meeting and spotting things that will be ambiguous later.

Return ONLY JSON: {\"questions\": [{\"question\": \"...\", \"about\": \"...\", \"kind\": \"...\"}]}

`kind` is one of: vague_reference, unquantified, unassigned_action, missing_deadline, \
undefined_term, contradiction, unstated_rationale.

Rules:
- At most 2 questions. Usually 0. An empty list is the correct answer most of the time.
- Only raise something a person in the room could answer in one sentence right now.
- `question` must be phrased so it can be read aloud verbatim, and be specific.
- `about` must quote the exact words from the transcript that prompted it.
- Do not ask about anything already clear from context.
- Do not ask generic meeting-hygiene questions such as \"what are the next steps\".
- Prefer silence over a weak question.";

/// Ask the model for clarifying questions about a window of transcript.
///
/// Implemented over [`AiBackend::chat`] rather than as a trait method so every backend —
/// including any OpenAI-compatible endpoint added later — supports it without extra code.
pub async fn suggest_questions(
    backend: &dyn AiBackend,
    window_text: &str,
    at_ms: i64,
) -> Result<Vec<ClarifyingQuestion>> {
    if window_text.trim().is_empty() {
        return Ok(Vec::new());
    }

    let request = ChatRequest::new(vec![ChatMessage::user(format!(
        "Recent transcript:\n\n{window_text}"
    ))])
    .with_context(vec![SYSTEM_PROMPT.to_string()]);

    let response = backend.chat(&request).await?;
    Ok(parse_questions(&response.text, at_ms))
}

/// Parse the model's reply into questions.
///
/// Deliberately lenient: a malformed reply means no suggestions this round, never an error.
/// A failed parse must not surface as an error banner in the middle of someone's meeting.
pub fn parse_questions(raw: &str, at_ms: i64) -> Vec<ClarifyingQuestion> {
    let cleaned = raw.trim();
    let cleaned = cleaned
        .strip_prefix("```json")
        .or_else(|| cleaned.strip_prefix("```"))
        .map(str::trim_start)
        .unwrap_or(cleaned);
    let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();

    // Some models add a sentence before the JSON; take the outermost object.
    let json = match (cleaned.find('{'), cleaned.rfind('}')) {
        (Some(start), Some(end)) if end > start => &cleaned[start..=end],
        _ => return Vec::new(),
    };

    #[derive(Deserialize)]
    struct Wrapper {
        #[serde(default)]
        questions: Vec<Raw>,
    }
    #[derive(Deserialize)]
    struct Raw {
        question: String,
        #[serde(default)]
        about: String,
        #[serde(default)]
        kind: String,
    }

    let Ok(wrapper) = serde_json::from_str::<Wrapper>(json) else {
        return Vec::new();
    };

    wrapper
        .questions
        .into_iter()
        .filter(|raw| !raw.question.trim().is_empty())
        .map(|raw| ClarifyingQuestion {
            question: raw.question.trim().to_string(),
            about: raw.about.trim().to_string(),
            // An unrecognized kind is still a usable question; default rather than drop it.
            kind: AmbiguityKind::parse(&raw.kind).unwrap_or(AmbiguityKind::VagueReference),
            at_ms,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MockBackend, Router, RouterConfig};

    fn question(text: &str, kind: AmbiguityKind, at_ms: i64) -> ClarifyingQuestion {
        ClarifyingQuestion {
            question: text.to_string(),
            about: "something said".into(),
            kind,
            at_ms,
        }
    }

    /// A window of transcript long enough to clear `min_window_chars`.
    fn transcript() -> Vec<Utterance> {
        vec![
            Utterance::new(
                "We should move the database over before the launch, it will be much faster.",
                0,
            )
            .by("Alex"),
            Utterance::new(
                "Agreed. Someone will need to handle the migration scripts and the index rebuild.",
                8_000,
            )
            .by("Sam"),
            Utterance::new(
                "Right, and we should do the other thing we discussed as well.",
                16_000,
            )
            .by("Alex"),
        ]
    }

    // ---------------------------------------------------------------- interruption policy

    #[test]
    fn stays_quiet_until_there_is_enough_context() {
        // Firing on the first sentence would make everything look ambiguous.
        let session = ClarifierSession::new(ClarifierConfig::default());
        let barely_started = vec![Utterance::new("Morning everyone.", 0)];

        assert!(!session.should_suggest(&barely_started, 1_000));
        assert!(session.should_suggest(&transcript(), 20_000));
    }

    #[test]
    fn respects_the_cooldown() {
        // Pestering is the failure that kills this feature.
        let mut session = ClarifierSession::new(ClarifierConfig::default());
        let transcript = transcript();

        assert!(session.should_suggest(&transcript, 20_000));
        session.accept(
            vec![question(
                "Which database?",
                AmbiguityKind::VagueReference,
                20_000,
            )],
            20_000,
        );

        assert!(
            !session.should_suggest(&transcript, 30_000),
            "10s after a suggestion is far too soon"
        );
        assert!(
            !session.should_suggest(&transcript, 100_000),
            "80s is still inside the 90s cooldown"
        );
        assert!(session.should_suggest(&transcript, 115_000));
    }

    #[test]
    fn the_cooldown_starts_even_when_every_question_is_a_duplicate() {
        // Otherwise a repeating ambiguity would trigger a model call on every single tick.
        let mut session = ClarifierSession::new(ClarifierConfig::default());
        let transcript = transcript();
        let q = question("Which database?", AmbiguityKind::VagueReference, 20_000);

        session.accept(vec![q.clone()], 20_000);
        let accepted = session.accept(vec![q], 25_000);

        assert!(accepted.is_empty(), "a repeat should not be surfaced");
        assert!(
            !session.should_suggest(&transcript, 30_000),
            "but it must still have started the cooldown"
        );
    }

    // ---------------------------------------------------------------- windowing

    #[test]
    fn the_window_excludes_old_utterances() {
        let session = ClarifierSession::new(ClarifierConfig::default());
        let transcript = vec![
            Utterance::new("Ancient history.", 0),
            Utterance::new("Recent thing.", 500_000),
        ];

        let window = session.window(&transcript, 520_000);
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].text, "Recent thing.");
    }

    #[test]
    fn window_text_is_speaker_prefixed() {
        let session = ClarifierSession::new(ClarifierConfig::default());
        let text = session.window_text(&transcript(), 20_000);

        assert!(text.contains("Alex: We should move the database"), "{text}");
        assert!(text.contains("Sam: Agreed."), "{text}");
    }

    #[test]
    fn an_empty_transcript_produces_an_empty_window() {
        let session = ClarifierSession::new(ClarifierConfig::default());
        assert!(session.window(&[], 10_000).is_empty());
        assert!(session.window_text(&[], 10_000).is_empty());
    }

    // ---------------------------------------------------------------- deduplication

    #[test]
    fn the_same_question_is_never_asked_twice() {
        let mut session = ClarifierSession::new(ClarifierConfig::default());

        let first = session.accept(
            vec![question(
                "Which database are we moving?",
                AmbiguityKind::VagueReference,
                0,
            )],
            0,
        );
        let second = session.accept(
            vec![question(
                "Which database are we moving?",
                AmbiguityKind::VagueReference,
                90_000,
            )],
            90_000,
        );

        assert_eq!(first.len(), 1);
        assert!(second.is_empty(), "a repeat destroys trust in the feature");
    }

    #[test]
    fn rewordings_of_one_question_collapse() {
        // The model will not phrase the same ambiguity identically twice, and a near
        // duplicate is just as irritating as an exact one.
        let a = question(
            "What is the timeline for the migration?",
            AmbiguityKind::MissingDeadline,
            0,
        );
        let b = question(
            "What's the migration timeline?",
            AmbiguityKind::MissingDeadline,
            0,
        );

        assert_eq!(
            a.dedupe_key(),
            b.dedupe_key(),
            "{} vs {}",
            a.dedupe_key(),
            b.dedupe_key()
        );
    }

    #[test]
    fn genuinely_different_questions_are_both_kept() {
        let mut session = ClarifierSession::new(ClarifierConfig::default());
        let accepted = session.accept(
            vec![
                question(
                    "Which database are we migrating?",
                    AmbiguityKind::VagueReference,
                    0,
                ),
                question(
                    "Who owns the index rebuild?",
                    AmbiguityKind::UnassignedAction,
                    0,
                ),
            ],
            0,
        );

        assert_eq!(accepted.len(), 2);
    }

    // ---------------------------------------------------------------- staleness & ranking

    #[test]
    fn stale_questions_are_dropped_unasked() {
        // Asking about something from fifteen minutes ago derails the conversation.
        let mut session = ClarifierSession::new(ClarifierConfig::default());
        session.accept(
            vec![question(
                "Which database?",
                AmbiguityKind::VagueReference,
                0,
            )],
            0,
        );
        assert_eq!(session.pending().len(), 1);

        session.prune(400_000);
        assert!(session.pending().is_empty());
    }

    #[test]
    fn the_costliest_questions_survive_the_cap() {
        let mut session = ClarifierSession::new(ClarifierConfig {
            max_pending: 2,
            ..Default::default()
        });

        session.accept(
            vec![
                question("What does CRDT mean?", AmbiguityKind::UndefinedTerm, 0),
                question(
                    "That contradicts the earlier plan — which holds?",
                    AmbiguityKind::Contradiction,
                    0,
                ),
                question("How much faster, roughly?", AmbiguityKind::Unquantified, 0),
                question(
                    "Who is doing the migration?",
                    AmbiguityKind::UnassignedAction,
                    0,
                ),
            ],
            0,
        );

        let kinds: Vec<_> = session.pending().iter().map(|q| q.kind).collect();
        assert_eq!(kinds.len(), 2);
        assert!(kinds.contains(&AmbiguityKind::Contradiction), "{kinds:?}");
        assert!(
            kinds.contains(&AmbiguityKind::UnassignedAction),
            "{kinds:?}"
        );
        assert!(!kinds.contains(&AmbiguityKind::UndefinedTerm), "{kinds:?}");
    }

    #[test]
    fn a_contradiction_outranks_a_definition() {
        // Two people building different things costs more than a term nobody defined.
        assert!(AmbiguityKind::Contradiction.weight() > AmbiguityKind::UndefinedTerm.weight());
        assert!(AmbiguityKind::UnassignedAction.weight() > AmbiguityKind::Unquantified.weight());
    }

    #[test]
    fn resolving_removes_a_question_from_the_panel() {
        let mut session = ClarifierSession::new(ClarifierConfig::default());
        let q = question("Which database?", AmbiguityKind::VagueReference, 0);
        session.accept(vec![q.clone()], 0);

        session.resolve(&q);
        assert!(session.pending().is_empty());
    }

    #[test]
    fn clearing_empties_the_panel_but_still_suppresses_repeats() {
        let mut session = ClarifierSession::new(ClarifierConfig::default());
        let q = question("Which database?", AmbiguityKind::VagueReference, 0);
        session.accept(vec![q.clone()], 0);

        session.clear();
        assert!(session.pending().is_empty());
        assert!(
            session.accept(vec![q], 200_000).is_empty(),
            "dismissing must not invite the same question back"
        );
    }

    // ---------------------------------------------------------------- parsing

    #[test]
    fn parses_a_well_formed_reply() {
        let raw = r#"{"questions":[
            {"question":"Which database are we migrating from?","about":"move the database over","kind":"vague_reference"},
            {"question":"Who owns the migration scripts?","about":"someone will need to","kind":"unassigned_action"}
        ]}"#;

        let parsed = parse_questions(raw, 5_000);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].kind, AmbiguityKind::VagueReference);
        assert_eq!(parsed[1].kind, AmbiguityKind::UnassignedAction);
        assert_eq!(parsed[0].at_ms, 5_000);
    }

    #[test]
    fn parses_a_fenced_reply() {
        let parsed = parse_questions(
            "```json\n{\"questions\":[{\"question\":\"Who owns it?\",\"kind\":\"unassigned_action\"}]}\n```",
            0,
        );
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn parses_json_buried_in_prose() {
        // Smaller local models routinely narrate before answering.
        let parsed = parse_questions(
            "Sure! Here is what I found:\n{\"questions\":[{\"question\":\"By when?\",\"kind\":\"missing_deadline\"}]}\nHope that helps.",
            0,
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].kind, AmbiguityKind::MissingDeadline);
    }

    #[test]
    fn an_empty_list_is_a_valid_answer() {
        // Silence is the correct output most of the time.
        assert!(parse_questions(r#"{"questions":[]}"#, 0).is_empty());
    }

    #[test]
    fn a_malformed_reply_yields_silence_not_an_error() {
        // An error banner mid-meeting is worse than no suggestion.
        for raw in ["I could not do that.", "", "{{{", "null", "[1,2,3]"] {
            assert!(parse_questions(raw, 0).is_empty(), "{raw:?}");
        }
    }

    #[test]
    fn an_unknown_kind_still_yields_a_usable_question() {
        let parsed = parse_questions(
            r#"{"questions":[{"question":"Which one?","kind":"interpretive_dance"}]}"#,
            0,
        );
        assert_eq!(parsed.len(), 1, "the question is still worth asking");
        assert_eq!(parsed[0].kind, AmbiguityKind::VagueReference);
    }

    #[test]
    fn blank_questions_are_discarded() {
        let parsed = parse_questions(
            r#"{"questions":[{"question":"   ","kind":"vague_reference"},{"question":"Real one?","kind":"vague_reference"}]}"#,
            0,
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].question, "Real one?");
    }

    #[test]
    fn every_kind_round_trips() {
        for kind in AmbiguityKind::ALL {
            assert_eq!(AmbiguityKind::parse(kind.as_str()), Some(*kind));
        }
        assert_eq!(AmbiguityKind::parse("nonsense"), None);
    }

    // ---------------------------------------------------------------- model call

    #[tokio::test]
    async fn an_empty_window_makes_no_model_call() {
        // Guarded by pointing at a failing backend: reaching it would be the bug.
        let backend = MockBackend::failing("must not be called");
        assert!(suggest_questions(&backend, "   ", 0)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn a_backend_error_propagates_rather_than_being_swallowed() {
        // A parse failure is silence; a broken backend is a real problem worth reporting.
        let backend = MockBackend::failing("provider down");
        assert!(suggest_questions(&backend, "some transcript", 0)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn works_through_a_router_over_any_backend() {
        // Built on chat(), so it works for every provider without per-backend code.
        let router = Router::from_config(RouterConfig::mock()).unwrap();
        // MockBackend returns prose, which correctly parses to no questions.
        let questions = suggest_questions(&router, "Alex: we should do the thing.", 0)
            .await
            .unwrap();
        assert!(questions.is_empty());
    }

    #[test]
    fn questions_round_trip_through_json() {
        let q = question("Who owns it?", AmbiguityKind::UnassignedAction, 1234);
        let json = serde_json::to_string(&q).unwrap();
        assert_eq!(
            serde_json::from_str::<ClarifyingQuestion>(&json).unwrap(),
            q
        );
    }
}
