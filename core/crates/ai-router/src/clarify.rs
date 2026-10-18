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
//!   feature feel broken, so a statement is asked about once, and a rewording of an earlier
//!   question is recognised by its content words.
//! - **A wrong question costs more than a missing one.** Every question must quote a line that
//!   was actually said, and that line must show the problem the question claims — see
//!   [`suggest_questions`].
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

        // Acronyms are kept whatever their length. The length rule below exists to drop "the",
        // "for" and "who", and it also dropped "ARB" — so "What is ARB?" had an empty key, and
        // `accept` discards an empty key as a question with no content. The one kind of question
        // made almost entirely of a short word was never shown.
        let acronyms: Vec<String> = self
            .question
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 2 && w.chars().all(|c| c.is_ascii_uppercase()))
            .map(str::to_lowercase)
            .collect();

        // Content words only: "what is the timeline for the migration" and "what's the
        // migration timeline" should collide.
        let mut words: Vec<&str> = normalized
            .split_whitespace()
            .filter(|w| (w.len() > 3 || acronyms.iter().any(|a| a == w)) && !STOPWORDS.contains(w))
            .collect();
        words.sort_unstable();
        words.dedup();
        words.join(" ")
    }

    pub fn is_stale(&self, now_ms: i64, staleness_ms: i64) -> bool {
        now_ms - self.at_ms > staleness_ms
    }

    /// Whether this asks what an earlier question asked, in other words.
    ///
    /// [`Self::dedupe_key`] equality only collapses rewordings that use exactly the same content
    /// words, and the model rarely obliges: "How many seats do we use today?" and "How many seats
    /// are we using?" differ by a word. Overlap of half the content words is the line — loose
    /// enough to catch that, tight enough that "who owns the index rebuild" and "who owns the
    /// migration scripts" stay two questions. Rewordings that share less than that are caught by
    /// the statement they quote instead; see [`statement_key`].
    fn rewords(&self, earlier_key: &str) -> bool {
        let key = self.dedupe_key();
        let mine = stems(&key);
        let theirs = stems(earlier_key);
        if mine.is_empty() || theirs.is_empty() {
            return false;
        }
        let shared = mine.intersection(&theirs).count();
        let union = mine.union(&theirs).count();
        shared * 2 >= union
    }
}

/// Content words with a plural `s` folded, so "seat" and "seats" count as one word.
fn stems(key: &str) -> std::collections::BTreeSet<&str> {
    key.split_whitespace()
        .map(|w| {
            w.strip_suffix('s')
                .filter(|stem| stem.len() > 3)
                .unwrap_or(w)
        })
        .collect()
}

/// The `asked` entry recording that a statement has already prompted a question.
///
/// Keyed on when the statement was said. Every poll sends the model the last two minutes, so
/// one statement is in front of it several times, and each time it finds a new way to ask about
/// it — measured on a live replay, one unowned task came back as three differently worded
/// questions over three polls. A statement gets one question.
///
/// Prefixed so it can never collide with a [`ClarifyingQuestion::dedupe_key`], which holds only
/// letters, digits and spaces.
fn statement_key(at_ms: i64) -> String {
    format!("@{at_ms}")
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

/// How many `asked` entries a resumed session keeps, newest last.
///
/// The list arrives from a client, so it is bounded rather than trusted: two entries per
/// question, and an hour-long meeting surfaces a few dozen at most.
const MAX_ASKED: usize = 400;

impl ClarifierSession {
    pub fn new(config: ClarifierConfig) -> Self {
        Self {
            config,
            pending: Vec::new(),
            asked: Vec::new(),
            last_suggestion_ms: None,
        }
    }

    /// Pick a session back up from the state a client carried between requests.
    ///
    /// The HTTP endpoint keeps nothing per meeting — two clients watching one meeting must not
    /// silence each other — so this policy only applies if the client hands its state back with
    /// each request. Rebuilt empty every time instead, the cooldown and every repeat check were
    /// dead code on the live path: the desktop app polls every thirty seconds, and each poll was
    /// a model call and, usually, a fresh rewording of a question it had already shown.
    ///
    /// A `last_suggestion_ms` later than any point the meeting has reached belongs to some other
    /// meeting, or a clock that has since been reset; honoured, it would hold the panel silent
    /// until the meeting caught up with it. It is ignored at the first check that sees it.
    pub fn resume(
        config: ClarifierConfig,
        mut asked: Vec<String>,
        last_suggestion_ms: Option<i64>,
    ) -> Self {
        if asked.len() > MAX_ASKED {
            asked.drain(..asked.len() - MAX_ASKED);
        }
        Self {
            config,
            pending: Vec::new(),
            asked,
            last_suggestion_ms,
        }
    }

    pub fn config(&self) -> ClarifierConfig {
        self.config
    }

    /// What has been asked so far, for a client to send back with its next request.
    pub fn asked(&self) -> &[String] {
        &self.asked
    }

    /// When the last model call was made, for a client to send back with its next request.
    pub fn last_suggestion_ms(&self) -> Option<i64> {
        self.last_suggestion_ms
    }

    /// Whether the cooldown is what stands between `now_ms` and the next suggestion.
    pub fn cooling_down(&self, now_ms: i64) -> bool {
        match self.last_suggestion_ms {
            Some(last) if last <= now_ms => now_ms - last < self.config.cooldown_ms,
            // In the future relative to now: see `resume`.
            _ => false,
        }
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

        !self.cooling_down(now_ms)
    }

    /// Render the window as a prompt fragment.
    pub fn window_text(&self, transcript: &[Utterance], now_ms: i64) -> String {
        render(self.window(transcript, now_ms))
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

        // Statements asked about before this batch. Not updated inside it: two questions about
        // one statement in the same reply are the model's to make, and `suggest_questions`
        // already keeps one per statement — this is about the *next* poll asking again.
        let earlier_statements: Vec<String> = self
            .asked
            .iter()
            .filter(|key| key.starts_with('@'))
            .cloned()
            .collect();

        let mut accepted = Vec::new();
        for question in questions {
            let key = question.dedupe_key();
            if key.is_empty()
                || earlier_statements.contains(&statement_key(question.at_ms))
                || self
                    .asked
                    .iter()
                    .any(|earlier| !earlier.starts_with('@') && question.rewords(earlier))
            {
                continue;
            }
            self.asked.push(key);
            self.asked.push(statement_key(question.at_ms));
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

/// The instructions for one look at the transcript.
///
/// # Why a checklist
///
/// The first version listed the seven kinds by name and asked for "usually 0" questions.
/// Replayed against `llama3.1:8b` on eight meetings it returned one or two questions on every
/// call — six apiece on two meetings with nothing ambiguous in them ("What does 'perfect' mean in
/// terms of weather?") — and filed five of six planted problems under the wrong kind. An open
/// "find what is ambiguous" invites a small model to find *something*.
///
/// Four narrow checks, each with its own slot that can be `null`, give it somewhere to say
/// "nothing" every time, and four narrow questions are ones a small model answers far better
/// than one broad one. They are the four costliest kinds. The vaguer three — an unclear
/// reference, a missing number, an unstated reason — produced most of the junk on the replay
/// and none of the useful questions, so they are no longer asked for; [`parse_questions`] still
/// reads them from a model that answers in the old shape.
///
/// What the model returns is checked rather than trusted: see [`ground`].
const SYSTEM_PROMPT: &str = "\
You are listening to a live meeting. Run four checks on the transcript, to catch what will \
cause confusion later unless someone asks about it now, while the people who can answer are \
still in the room.

1. \"owner\": work that has to be done but that no person took on. Look for \"someone needs \
to\", \"somebody should\", \"we need to\", or a decision that creates work nobody was given. If \
anyone said they will do it (\"I'll\", \"I can\", \"Sam is doing it\") or was asked by name, it \
is null.
2. \"date\": a commitment whose timing is vague when an exact day matters: \"soon\", \"later\", \
\"next week\", \"end of next week\", \"this quarter\", \"at some point\". If an exact day or time \
is given, it is null.
3. \"conflict\": compare every amount, count and date with the ones said earlier. If the same \
budget, number or date is given two different ways, quote the later line and the earlier line.
4. \"term\": an acronym or internal name that the plan depends on and that nobody explained. \
Everyday acronyms (SSO, CSV, QA, API, PDF) are null.

Most transcripts have none of these. A check is null unless the transcript clearly shows the \
problem. Small talk, things already answered, and what ordinary words mean are always null.

For each problem found, write a question someone could read aloud to the room: one short \
sentence that names the specific task, figure or term. \"quote\" is copied word for word from \
ONE line of the transcript.

Return ONLY JSON in exactly this shape:
{\"owner\": null or {\"quote\": \"...\", \"question\": \"...\"}, \"date\": null or {\"quote\": \
\"...\", \"question\": \"...\"}, \"conflict\": null or {\"quote\": \"...\", \"earlier\": \"...\", \
\"question\": \"...\"}, \"term\": null or {\"quote\": \"...\", \"question\": \"...\"}}

Example transcript:
Ana: Someone has to update the on-call runbook before the audit.
Ben: Yes. Moving on, the hiring plan.
Answer: {\"owner\": {\"quote\": \"Someone has to update the on-call runbook before the audit.\", \
\"question\": \"Who will update the on-call runbook, and by when?\"}, \"date\": null, \
\"conflict\": null, \"term\": null}

Example transcript:
Kim: We are hiring twelve engineers this year.
Lee: Good. The offers go out Monday.
Kim: With fifteen new engineers, we need a second team lead.
Answer: {\"owner\": null, \"date\": null, \"conflict\": {\"quote\": \"With fifteen new engineers, \
we need a second team lead.\", \"earlier\": \"We are hiring twelve engineers this year.\", \
\"question\": \"Are we hiring twelve engineers this year or fifteen?\"}, \"term\": null}

Example transcript:
Ana: I'll send the draft to legal on Tuesday the 3rd.
Ben: And I'll review their comments on Thursday the 5th.
Answer: {\"owner\": null, \"date\": null, \"conflict\": null, \"term\": null}";

/// The checklist's slots, and the kind each one reports.
const SLOTS: &[(&str, AmbiguityKind)] = &[
    ("owner", AmbiguityKind::UnassignedAction),
    ("date", AmbiguityKind::MissingDeadline),
    ("conflict", AmbiguityKind::Contradiction),
    ("term", AmbiguityKind::UndefinedTerm),
];

/// Most questions one look at the transcript may produce.
///
/// Small local models do not reliably stop where they are told to. Enforced here, highest cost
/// first, so a reply listing five cannot put five cards in front of someone mid-sentence.
const MAX_PER_CALL: usize = 2;

/// Ask the model for clarifying questions about a window of transcript.
///
/// Implemented over [`AiBackend::chat`] rather than as a trait method so every backend —
/// including any OpenAI-compatible endpoint added later — supports it without extra code.
///
/// Takes the utterances rather than rendered text so every question can be checked against the
/// line that prompted it — see `ground` — and the rendering lives in one place.
pub async fn suggest_questions(
    backend: &dyn AiBackend,
    window: &[Utterance],
) -> Result<Vec<ClarifyingQuestion>> {
    let window_text = render(window);
    if window_text.trim().is_empty() {
        return Ok(Vec::new());
    }

    let request = ChatRequest::new(vec![ChatMessage::user(format!(
        "Recent transcript:\n\n{window_text}"
    ))])
    .with_context(vec![SYSTEM_PROMPT.to_string()]);

    let response = backend.chat(&request).await?;
    Ok(ground(parse_candidates(&response.text), window))
}

/// One utterance per line, speaker first, as the model sees the window.
fn render(window: &[Utterance]) -> String {
    window
        .iter()
        .map(|u| match &u.speaker {
            Some(speaker) => format!("{speaker}: {}", u.text),
            None => u.text.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A question as the model proposed it, before it is checked against the transcript.
#[derive(Debug, Clone)]
struct Candidate {
    question: ClarifyingQuestion,
    /// For a contradiction, the earlier line it conflicts with.
    earlier: Option<String>,
}

/// Check each proposed question against the transcript, and keep only the ones that hold.
///
/// # The quote has to be real
///
/// The panel shows `about` in quotation marks, beneath the question, as words someone in the
/// room said. A quote the model invented or blended from two lines puts words in a colleague's
/// mouth, so a question whose quote is not in the window is discarded rather than shown. One that
/// survives keeps a verbatim quote — the model's, when it copied faithfully, otherwise the line.
///
/// It also takes that line's time. `at_ms` is documented as when the prompting statement was
/// said, and staleness is measured from it, but every question used to be stamped with the time
/// of the poll — so something said two minutes earlier arrived looking brand new.
///
/// # The quote has to show the problem
///
/// Most of the junk on the replay was a real quote under the wrong claim: "who owns QA?" beside
/// "Ines owns that", "when is it due?" beside "Thursday the 9th". Each kind names something a line
/// either shows or does not — see [`evidence_holds`] — and a question whose own quote contradicts
/// it is dropped. The checks are English phrase lists, so they apply only when the window reads
/// as English; anywhere else they would silence every question, and grounding alone stands.
///
/// # Then the cap
///
/// One question per statement — two about one line are two phrasings of one problem — highest
/// cost first, at most [`MAX_PER_CALL`].
fn ground(candidates: Vec<Candidate>, window: &[Utterance]) -> Vec<ClarifyingQuestion> {
    let checks_apply = reads_as_english(window);

    let mut grounded: Vec<ClarifyingQuestion> = candidates
        .into_iter()
        .filter_map(|candidate| {
            let Candidate {
                mut question,
                earlier,
            } = candidate;
            let (line, faithful) = locate(&question.about, window)?;
            let earlier = earlier
                .as_deref()
                .and_then(|quote| locate(quote, window))
                .map(|(earlier, _)| earlier);

            if checks_apply && !evidence_holds(&question, line, earlier, window) {
                return None;
            }

            if !faithful {
                question.about = line.text.trim().to_string();
            }
            question.at_ms = line.at_ms;
            Some(question)
        })
        .collect();

    // Stable, so equal-cost questions keep the model's order.
    grounded.sort_by_key(|q| std::cmp::Reverse(q.kind.weight()));
    let mut seen = Vec::new();
    grounded.retain(|q| {
        if seen.contains(&q.at_ms) {
            return false;
        }
        seen.push(q.at_ms);
        true
    });
    grounded.truncate(MAX_PER_CALL);
    grounded
}

/// Whether the line a question quotes shows the problem its kind claims.
///
/// Deliberately lexical and deliberately one-sided: each rule rejects a question whose evidence
/// plainly does not support it, and none tries to find problems on its own. Finding is the
/// model's job; these only stop it asserting something its own quote disproves.
fn evidence_holds(
    question: &ClarifyingQuestion,
    line: &Utterance,
    earlier: Option<&Utterance>,
    window: &[Utterance],
) -> bool {
    let text = &line.text;
    match question.kind {
        // Somebody has to be missing. The line either says so ("someone needs to") or follows a
        // decision ("decision made" … "that means a migration plan"), and nobody takes the work
        // on — in that line, or in the reply to it: "Ines owns that", "I'll review it today".
        AmbiguityKind::UnassignedAction => {
            let position = window.iter().position(|u| std::ptr::eq(u, line));
            let before = position.and_then(|i| i.checked_sub(1)).map(|i| &window[i]);
            let after = position.and_then(|i| window.get(i + 1));

            let unowned = mentions_any(text, OWNERLESS)
                || mentions_any(text, DECIDED)
                || before.is_some_and(|b| mentions_any(&b.text, DECIDED));
            let taken = mentions_any(text, TAKEN_ON)
                || after.is_some_and(|a| mentions_any(&a.text, TAKEN_ON));
            unowned && !taken
        }
        // The timing has to be vague, and not pinned down in the same breath.
        AmbiguityKind::MissingDeadline => mentions_any(text, VAGUE_TIME) && !names_a_day(text),
        // Two lines, each with a figure the other lacks: "forty thousand" then "sixty thousand".
        // One line cannot contradict itself, and "the 21st" beside "the 21st at 9am" is a
        // refinement rather than a conflict.
        AmbiguityKind::Contradiction => earlier.is_some_and(|earlier| {
            if earlier.at_ms == line.at_ms {
                return false;
            }
            let (then, now) = (figures(&earlier.text), figures(text));
            !then.is_empty()
                && !now.is_empty()
                && then.difference(&now).next().is_some()
                && now.difference(&then).next().is_some()
        }),
        // There has to be an unexplained acronym in the line, and the question has to be
        // about it: "what does 'perfect' mean" quotes no acronym at all.
        AmbiguityKind::UndefinedTerm => {
            acronyms(text).any(|acronym| question.question.contains(acronym))
        }
        AmbiguityKind::VagueReference => mentions_any(text, VAGUE_REFERENCE),
        AmbiguityKind::Unquantified => mentions_any(text, COMPARATIVE) && figures(text).is_empty(),
        AmbiguityKind::UnstatedRationale => {
            mentions_any(text, DECIDED) && !mentions_any(text, BECAUSE)
        }
    }
}

/// Phrases that leave a task without anyone attached. Normalized: lowercase, no apostrophes.
const OWNERLESS: &[&str] = &[
    // Not "anyone": it is how a room is asked a question ("is anything blocking anyone?"), not
    // how a task is left without an owner.
    "someone",
    "somebody",
    "no one",
    "nobody",
    "we need",
    "we should",
    "we have to",
    "we must",
    "we ll need",
    "we will need",
    "needs to",
    "need to be",
    "has to",
    "have to",
    "has got to",
    "must be",
    "should be",
    "circle back",
    "come back to",
    "park that",
    "take it offline",
    "table that",
    "tbd",
    "to be decided",
    "to be determined",
];

/// Phrases in which somebody takes the task.
const TAKEN_ON: &[&str] = &[
    "i ll",
    "i will",
    "i can",
    "i m going to",
    "i am going to",
    "let me",
    "i m on it",
    "i own",
    "owns",
    "i m doing",
    "i am doing",
    "is doing it",
    "is on it",
    "will do it",
    "assigned to",
];

/// Timing too loose to plan against.
const VAGUE_TIME: &[&str] = &[
    "soon",
    "sooner",
    "later",
    "next week",
    "next month",
    "next quarter",
    "this quarter",
    "end of the week",
    "end of next week",
    "end of the month",
    "end of the quarter",
    "end of the year",
    "some point",
    "sometime",
    "some time",
    "eventually",
    "asap",
    "as soon as",
    "shortly",
    "in a bit",
    "in a few",
    "a couple of",
    "few weeks",
    "few days",
    "coming weeks",
    "down the road",
    "before long",
    "whenever",
];

const VAGUE_REFERENCE: &[&str] = &[
    "that thing",
    "the other thing",
    "that approach",
    "the other approach",
    "that idea",
    "what we discussed",
    "we discussed",
    "what we talked about",
    "the usual",
    "same as before",
    "same as last time",
    "that one",
    "the other one",
    "that stuff",
];

const COMPARATIVE: &[&str] = &[
    "faster",
    "slower",
    "cheaper",
    "more expensive",
    "a lot",
    "lots",
    "much more",
    "much less",
    "way more",
    "way less",
    "significantly",
    "huge",
    "massive",
    "tiny",
    "bigger",
    "smaller",
    "better",
    "worse",
    "higher",
    "lower",
    "fewer",
    "many",
    "several",
    "a bunch",
];

const DECIDED: &[&str] = &[
    "we decided",
    "decided to",
    "decision made",
    "we re going with",
    "we are going with",
    "let s go with",
    "we ll go with",
    "going with",
    "we chose",
    "we picked",
    "agreed to",
];

const BECAUSE: &[&str] = &[
    "because",
    "since",
    "so that",
    "due to",
    "the reason",
    "given that",
    "which means",
];

const WEEKDAYS: &[&str] = &[
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// Month names. "May" is left out: as a word it is far more often the verb.
const MONTHS: &[&str] = &[
    "january",
    "february",
    "march",
    "april",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "jan",
    "feb",
    "mar",
    "apr",
    "jun",
    "jul",
    "aug",
    "sep",
    "sept",
    "oct",
    "nov",
    "dec",
];

const NUMBER_WORDS: &[&str] = &[
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "thousand",
    "million",
    "billion",
    "half",
    "dozen",
];

/// Acronyms so common that nobody in a meeting needs them explained.
const EVERYDAY_ACRONYMS: &[&str] = &[
    "AI", "AM", "PM", "API", "APIS", "CEO", "CFO", "COO", "CTO", "VP", "CSV", "PDF", "URL", "UI",
    "UX", "QA", "HR", "PR", "IT", "OK", "ETA", "FAQ", "FYI", "ASAP", "SSO", "SQL", "KPI", "ROI",
    "SLA", "MVP", "US", "UK", "EU", "USA", "TV", "ID", "IOS", "AWS", "GCP", "CRM", "SAAS", "Q1",
    "Q2", "Q3", "Q4", "H1", "H2", "FY", "EOD", "EOW", "TBD", "PT", "PST", "ET", "EST", "GMT",
    "UTC", "OKR", "OKRS", "PTO", "SMS", "IP", "VPN", "DNS",
];

/// Common English words, for telling whether the phrase lists above can mean anything.
const ENGLISH: &[&str] = &[
    "the", "and", "to", "of", "a", "in", "is", "we", "it", "that", "for", "on", "with", "you", "i",
    "this", "be", "are", "was", "so", "but", "have", "will", "our", "they", "at", "not",
];

/// Whether `text` contains any of `phrases`, as whole words.
fn mentions_any(text: &str, phrases: &[&str]) -> bool {
    let padded = format!(" {} ", normalize(text));
    phrases
        .iter()
        .any(|phrase| padded.contains(&format!(" {phrase} ")))
}

/// Whether a line pins a time down: a weekday, a month, an ordinal day, a clock time.
fn names_a_day(text: &str) -> bool {
    let normalized = normalize(text);
    normalized.split(' ').any(|word| {
        WEEKDAYS.contains(&word)
            || MONTHS.contains(&word)
            || ["today", "tonight", "tomorrow", "noon", "midnight", "eod"].contains(&word)
            || is_ordinal_or_time(word)
    })
}

/// "21st", "3rd", "9am", "5pm".
fn is_ordinal_or_time(word: &str) -> bool {
    let digits = word.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && matches!(&word[digits..], "st" | "nd" | "rd" | "th" | "am" | "pm")
}

/// The figures a line states: numbers in digits or words, weekdays and months.
fn figures(text: &str) -> std::collections::BTreeSet<String> {
    normalize(text)
        .split(' ')
        .filter(|word| {
            word.chars().any(|c| c.is_ascii_digit())
                || NUMBER_WORDS.contains(word)
                || WEEKDAYS.contains(word)
                || MONTHS.contains(word)
        })
        .map(str::to_string)
        .collect()
}

/// Tokens that look like an acronym nobody would know without being told.
fn acronyms(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| (2..=6).contains(&token.len()))
        .filter(|token| token.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
        .filter(|token| {
            token
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && token.chars().filter(char::is_ascii_uppercase).count() >= 2
        })
        .filter(|token| !EVERYDAY_ACRONYMS.contains(token))
}

/// Whether the window is English enough for the phrase lists to mean anything.
///
/// One word in five being among the commonest English words is a low bar that English clears
/// easily and other languages do not. Too little text to judge counts as English: the checks
/// are the cautious side, and a window that short rarely reaches the model anyway.
fn reads_as_english(window: &[Utterance]) -> bool {
    let text = normalize(&render(window));
    let words: Vec<&str> = text.split(' ').filter(|w| !w.is_empty()).collect();
    if words.len() < 20 {
        return true;
    }
    let common = words.iter().filter(|w| ENGLISH.contains(w)).count();
    common * 5 >= words.len()
}

/// The utterance a quote came from, and whether the quote is faithful to it.
///
/// Faithful means the quote's words appear in the line in order, ignoring case and
/// punctuation — what a model produces when it copies. Failing that, a line holding at least
/// four-fifths of the quote's words is accepted as the source of a loose paraphrase, and the
/// caller shows the line instead. Anything less is not a quote.
///
/// A leading "Name:" is ignored: the window is rendered speaker-first, and models copy the label
/// along with the words.
fn locate<'a>(quote: &str, window: &'a [Utterance]) -> Option<(&'a Utterance, bool)> {
    let mut quote = normalize(quote);
    for utterance in window {
        if let Some(speaker) = &utterance.speaker {
            let label = normalize(speaker);
            if let Some(rest) = quote.strip_prefix(&format!("{label} ")) {
                quote = rest.to_string();
                break;
            }
        }
    }
    if quote.is_empty() {
        return None;
    }

    let padded = |text: &str| format!(" {} ", normalize(text));
    if let Some(utterance) = window
        .iter()
        .find(|u| padded(&u.text).contains(&format!(" {quote} ")))
    {
        return Some((utterance, true));
    }

    let words: Vec<&str> = quote.split(' ').collect();
    if words.len() < 3 {
        return None;
    }
    window
        .iter()
        .map(|u| {
            let line = normalize(&u.text);
            let have: std::collections::BTreeSet<&str> = line.split(' ').collect();
            let found = words.iter().filter(|w| have.contains(**w)).count();
            (u, found)
        })
        .filter(|(_, found)| found * 5 >= words.len() * 4)
        .max_by_key(|(_, found)| *found)
        .map(|(u, _)| (u, false))
}

/// Lowercase words separated by single spaces, punctuation dropped.
fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse the model's reply into questions.
///
/// Reads both shapes a model may answer in: the checklist `SYSTEM_PROMPT` asks for, and a
/// plain `{"questions": [...]}` list, which some models fall back to whatever they are told.
///
/// Deliberately lenient: a malformed reply means no suggestions this round, never an error.
/// A failed parse must not surface as an error banner in the middle of someone's meeting.
pub fn parse_questions(raw: &str, at_ms: i64) -> Vec<ClarifyingQuestion> {
    parse_candidates(raw)
        .into_iter()
        .map(|candidate| ClarifyingQuestion {
            at_ms,
            ..candidate.question
        })
        .collect()
}

fn parse_candidates(raw: &str) -> Vec<Candidate> {
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

    let Ok(serde_json::Value::Object(reply)) = serde_json::from_str::<serde_json::Value>(json)
    else {
        return Vec::new();
    };

    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let candidate = |value: &serde_json::Value, kind: AmbiguityKind| {
        let question = text(value, "question");
        if question.is_empty() {
            return None;
        }
        // `quote` in the checklist, `about` in the list shape.
        let mut about = text(value, "quote");
        if about.is_empty() {
            about = text(value, "about");
        }
        let earlier = Some(text(value, "earlier")).filter(|e| !e.is_empty());
        Some(Candidate {
            question: ClarifyingQuestion {
                question,
                about,
                kind,
                at_ms: 0,
            },
            earlier,
        })
    };

    if let Some(list) = reply.get("questions").and_then(serde_json::Value::as_array) {
        return list
            .iter()
            .filter_map(|item| {
                // An unrecognized kind is still a usable question; default rather than drop it.
                let kind = AmbiguityKind::parse(&text(item, "kind"))
                    .unwrap_or(AmbiguityKind::VagueReference);
                candidate(item, kind)
            })
            .collect();
    }

    SLOTS
        .iter()
        .filter_map(|(slot, kind)| reply.get(*slot).and_then(|value| candidate(value, *kind)))
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
        assert!(suggest_questions(&backend, &[]).await.unwrap().is_empty());
        assert!(suggest_questions(&backend, &[Utterance::new("   ", 0)])
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn a_backend_error_propagates_rather_than_being_swallowed() {
        // A parse failure is silence; a broken backend is a real problem worth reporting.
        let backend = MockBackend::failing("provider down");
        assert!(
            suggest_questions(&backend, &[Utterance::new("some transcript", 0)])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn works_through_a_router_over_any_backend() {
        // Built on chat(), so it works for every provider without per-backend code.
        let router = Router::from_config(RouterConfig::mock()).unwrap();
        // MockBackend returns prose, which correctly parses to no questions.
        let window = [Utterance::new("we should do the thing.", 0).by("Alex")];
        let questions = suggest_questions(&router, &window).await.unwrap();
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

    // ---------------------------------------------------------------- a session carried by its client

    /// The endpoint is stateless; the client carries the session. Rebuilt empty on every request,
    /// the cooldown never applied and nothing asked was remembered.
    #[test]
    fn a_resumed_session_keeps_its_cooldown_and_its_memory() {
        let mut first = ClarifierSession::new(ClarifierConfig::default());
        let q = question(
            "Who owns the index rebuild?",
            AmbiguityKind::UnassignedAction,
            8_000,
        );
        first.accept(vec![q.clone()], 20_000);

        let mut resumed = ClarifierSession::resume(
            ClarifierConfig::default(),
            first.asked().to_vec(),
            first.last_suggestion_ms(),
        );
        assert!(resumed.cooling_down(50_000));
        assert!(!resumed.should_suggest(&transcript(), 50_000));
        assert!(resumed.should_suggest(&transcript(), 115_000));
        assert!(
            resumed.accept(vec![q], 115_000).is_empty(),
            "a question the client was already shown must not come back"
        );
    }

    /// State from another meeting, or from before a clock reset, would otherwise hold the panel
    /// silent until the meeting caught up with it.
    #[test]
    fn a_last_suggestion_from_the_future_does_not_silence_the_meeting() {
        let session =
            ClarifierSession::resume(ClarifierConfig::default(), Vec::new(), Some(3_000_000));
        assert!(!session.cooling_down(20_000));
        assert!(session.should_suggest(&transcript(), 20_000));
    }

    #[test]
    fn a_resumed_session_bounds_what_it_remembers() {
        let asked: Vec<String> = (0..1_000).map(|n| format!("@{n}")).collect();
        let session = ClarifierSession::resume(ClarifierConfig::default(), asked, None);
        assert_eq!(session.asked().len(), MAX_ASKED);
        assert_eq!(
            session.asked().last().map(String::as_str),
            Some("@999"),
            "the newest are kept"
        );
    }

    // ---------------------------------------------------------------- repeats across polls

    /// Measured on a live replay: one "someone needs to" line, in front of the model on three
    /// polls, became three differently worded questions.
    #[test]
    fn a_statement_is_asked_about_once_however_the_question_is_worded() {
        let mut session = ClarifierSession::new(ClarifierConfig::default());
        let first = session.accept(
            vec![question(
                "Who will pull the usage report?",
                AmbiguityKind::UnassignedAction,
                39_000,
            )],
            50_000,
        );
        let later = session.accept(
            vec![question(
                "When must the report reach their account manager?",
                AmbiguityKind::MissingDeadline,
                39_000,
            )],
            140_000,
        );
        assert_eq!(first.len(), 1);
        assert!(later.is_empty(), "same statement, new wording: {later:?}");
    }

    #[test]
    fn rewordings_that_share_most_of_their_words_collapse() {
        let mut session = ClarifierSession::new(ClarifierConfig::default());
        session.accept(
            vec![question(
                "How many seats do we use today?",
                AmbiguityKind::Unquantified,
                1_000,
            )],
            10_000,
        );
        let again = session.accept(
            vec![question(
                "How many seats are we using?",
                AmbiguityKind::Unquantified,
                30_000,
            )],
            100_000,
        );
        assert!(again.is_empty(), "{again:?}");
    }

    /// The length rule that drops "the" and "who" also dropped "ARB", leaving "What is ARB?" with
    /// an empty key — and an empty key is discarded as a question with no content.
    #[test]
    fn a_question_about_a_short_acronym_is_not_discarded() {
        let mut session = ClarifierSession::new(ClarifierConfig::default());
        let accepted = session.accept(
            vec![question(
                "What is ARB?",
                AmbiguityKind::UndefinedTerm,
                26_000,
            )],
            50_000,
        );
        assert_eq!(accepted.len(), 1);

        let reworded = session.accept(
            vec![question(
                "What does ARB stand for?",
                AmbiguityKind::UndefinedTerm,
                60_000,
            )],
            140_000,
        );
        assert!(reworded.is_empty(), "and its rewording is still a repeat");
    }

    // ---------------------------------------------------------------- grounding

    fn proposed(text: &str, about: &str, kind: AmbiguityKind) -> Candidate {
        Candidate {
            question: ClarifyingQuestion {
                question: text.into(),
                about: about.into(),
                kind,
                at_ms: 0,
            },
            earlier: None,
        }
    }

    fn meeting(lines: &[(&str, &str)]) -> Vec<Utterance> {
        lines
            .iter()
            .enumerate()
            .map(|(i, (speaker, text))| Utterance::new(*text, i as i64 * 13_000).by(*speaker))
            .collect()
    }

    /// The panel shows the quote as words someone said. One that is not in the transcript puts
    /// words in a colleague's mouth.
    #[test]
    fn a_question_whose_quote_is_not_in_the_window_is_dropped() {
        let window = meeting(&[("Ana", "Someone has to update the runbook before the audit.")]);
        let kept = ground(
            vec![proposed(
                "Who will email the auditors?",
                "somebody should email the auditors",
                AmbiguityKind::UnassignedAction,
            )],
            &window,
        );
        assert!(kept.is_empty(), "{kept:?}");
    }

    /// `at_ms` is when the statement was said. It used to be the time of the poll.
    #[test]
    fn a_question_takes_the_time_of_the_line_it_quotes() {
        let window = meeting(&[
            ("Priya", "The contract renews on November 30th."),
            ("Marcus", "Someone needs to pull the usage report."),
            ("Priya", "Moving on to the survey results."),
        ]);
        let kept = ground(
            vec![proposed(
                "Who will pull the usage report?",
                // The label is copied along with the words; that is still a faithful quote.
                "Marcus: Someone needs to pull the usage report",
                AmbiguityKind::UnassignedAction,
            )],
            &window,
        );
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].at_ms, 13_000);
    }

    #[test]
    fn a_loose_paraphrase_is_shown_as_the_line_itself() {
        let window = [
            Utterance::new("Okay, decision made: we're moving onto Postgres.", 0).by("Kofi"),
            Utterance::new(
                "Great. That means a migration plan, a data backfill, and rewriting the data \
                 access layer.",
                13_000,
            )
            .by("Mia"),
        ];
        let kept = ground(
            vec![proposed(
                "Who will own the migration plan and the backfill?",
                "we need a migration plan, a data backfill, and rewriting the data access layer",
                AmbiguityKind::UnassignedAction,
            )],
            &window,
        );
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].about, window[1].text, "the words actually said");
    }

    // ---------------------------------------------------------------- evidence

    /// The commonest junk on the replay: a real quote under a claim it disproves.
    #[test]
    fn an_owner_question_about_a_task_somebody_took_is_dropped() {
        let window = meeting(&[
            ("Ravi", "QA is on Monday the 13th, and Ines owns that."),
            ("Lena", "We need final copy approval on the invite emails."),
            ("Omar", "I'll review the copy this afternoon."),
        ]);
        let kept = ground(
            vec![
                proposed(
                    "Who owns QA on Monday?",
                    "Ines owns that",
                    AmbiguityKind::UnassignedAction,
                ),
                proposed(
                    "Who will approve the invite copy?",
                    "We need final copy approval on the invite emails.",
                    AmbiguityKind::UnassignedAction,
                ),
            ],
            &window,
        );
        assert!(kept.is_empty(), "both were taken on: {kept:?}");
    }

    #[test]
    fn an_owner_question_about_work_nobody_took_is_kept() {
        let window = meeting(&[
            ("Marcus", "Someone needs to pull the usage report."),
            ("Priya", "Yes, before the 30th or we lose the leverage."),
        ]);
        let kept = ground(
            vec![proposed(
                "Who will pull the usage report?",
                "Someone needs to pull the usage report.",
                AmbiguityKind::UnassignedAction,
            )],
            &window,
        );
        assert_eq!(kept.len(), 1);
    }

    /// A decision creates work without anyone saying "someone needs to".
    #[test]
    fn work_that_follows_a_decision_counts_as_unowned() {
        let window = meeting(&[
            (
                "Kofi",
                "Okay, decision made: we're moving off Mongo and onto Postgres.",
            ),
            ("Mia", "That means a migration plan and a data backfill."),
            ("Ben", "Next topic is the on-call rotation."),
        ]);
        let kept = ground(
            vec![proposed(
                "Who will write the migration plan?",
                "That means a migration plan and a data backfill.",
                AmbiguityKind::UnassignedAction,
            )],
            &window,
        );
        assert_eq!(kept.len(), 1, "{kept:?}");
    }

    #[test]
    fn a_deadline_question_needs_vague_timing_and_no_exact_day() {
        let window = meeting(&[
            (
                "Omar",
                "Sam says the landing page will be live by end of next week.",
            ),
            ("Hana", "The endpoint ships on Thursday the 9th."),
        ]);
        let kept = ground(
            vec![
                proposed(
                    "What date will the landing page be live?",
                    "live by end of next week",
                    AmbiguityKind::MissingDeadline,
                ),
                proposed(
                    "When does the endpoint ship?",
                    "The endpoint ships on Thursday the 9th.",
                    AmbiguityKind::MissingDeadline,
                ),
            ],
            &window,
        );
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(kept[0].question.contains("landing page"));
    }

    #[test]
    fn a_contradiction_needs_two_lines_with_different_figures() {
        let window = meeting(&[
            (
                "Dana",
                "We have forty thousand dollars in the events budget for Q3.",
            ),
            (
                "Dana",
                "With the sixty thousand we have for events, we can do the dinner too.",
            ),
            ("Raj", "The release is on the 21st."),
            ("Sofia", "The blog post goes out on the 21st at 9am."),
        ]);
        let conflict = |quote: &str, earlier: Option<&str>| Candidate {
            earlier: earlier.map(str::to_string),
            ..proposed("Which figure holds?", quote, AmbiguityKind::Contradiction)
        };

        let kept = ground(
            vec![conflict(
                "With the sixty thousand we have for events",
                Some("We have forty thousand dollars in the events budget for Q3."),
            )],
            &window,
        );
        assert_eq!(kept.len(), 1, "forty, then sixty: {kept:?}");

        for (quote, earlier) in [
            // No earlier line at all.
            ("With the sixty thousand we have for events", None),
            // One line cannot contradict itself.
            (
                "With the sixty thousand we have for events",
                Some("With the sixty thousand we have for events"),
            ),
            // A refinement, not a conflict.
            (
                "The blog post goes out on the 21st at 9am.",
                Some("The release is on the 21st."),
            ),
        ] {
            assert!(
                ground(vec![conflict(quote, earlier)], &window).is_empty(),
                "{quote} / {earlier:?}"
            );
        }
    }

    #[test]
    fn a_term_question_has_to_name_an_unexplained_acronym_in_the_line() {
        let window = meeting(&[
            (
                "Chen",
                "The design still has to get through ARB before production.",
            ),
            ("Ben", "The weather was perfect, and SSO is already on."),
        ]);
        let kept = ground(
            vec![
                proposed("What is ARB?", "ARB", AmbiguityKind::UndefinedTerm),
                proposed(
                    "What does 'perfect' mean?",
                    "The weather was perfect",
                    AmbiguityKind::UndefinedTerm,
                ),
                proposed(
                    "What is SSO?",
                    "SSO is already on",
                    AmbiguityKind::UndefinedTerm,
                ),
            ],
            &window,
        );
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].question, "What is ARB?");
    }

    /// The checks are English phrase lists. Applied to another language they would reject every
    /// question, so only grounding applies there.
    #[test]
    fn the_evidence_checks_stand_aside_for_other_languages() {
        let window = meeting(&[
            (
                "Jonas",
                "Jemand muss den Nutzungsbericht aus der Admin-Konsole ziehen und an den \
                 Kundenbetreuer schicken.",
            ),
            (
                "Petra",
                "Ja, und zwar deutlich vor dem Dreißigsten, sonst verlieren wir unseren \
                 Spielraum bei den Lizenzen.",
            ),
        ]);
        let kept = ground(
            vec![proposed(
                "Wer zieht den Nutzungsbericht?",
                "Jemand muss den Nutzungsbericht aus der Admin-Konsole ziehen",
                AmbiguityKind::UnassignedAction,
            )],
            &window,
        );
        assert_eq!(kept.len(), 1, "{kept:?}");
    }

    // ---------------------------------------------------------------- the cap

    #[test]
    fn one_look_yields_at_most_two_questions_costliest_first_one_per_statement() {
        let window = meeting(&[
            ("A", "Someone needs to book the venue."),
            ("B", "The design has to clear ARB first."),
            ("C", "We have twelve seats booked, says the sheet."),
            (
                "D",
                "With fifteen seats booked we need a bigger room, and somebody should ask \
                 facilities.",
            ),
        ]);
        let mut contradiction = proposed(
            "Twelve seats or fifteen?",
            "With fifteen seats booked we need a bigger room",
            AmbiguityKind::Contradiction,
        );
        contradiction.earlier = Some("We have twelve seats booked".into());

        let kept = ground(
            vec![
                proposed(
                    "What is ARB?",
                    "The design has to clear ARB first.",
                    AmbiguityKind::UndefinedTerm,
                ),
                proposed(
                    "Who will book the venue?",
                    "Someone needs to book the venue.",
                    AmbiguityKind::UnassignedAction,
                ),
                contradiction,
                // The same statement as the contradiction: one question per statement.
                proposed(
                    "Who will ask facilities?",
                    "somebody should ask facilities",
                    AmbiguityKind::UnassignedAction,
                ),
            ],
            &window,
        );

        let kinds: Vec<_> = kept.iter().map(|q| q.kind).collect();
        assert_eq!(
            kinds,
            vec![
                AmbiguityKind::Contradiction,
                AmbiguityKind::UnassignedAction
            ],
            "{kept:?}"
        );
        assert_eq!(kept[1].question, "Who will book the venue?");
    }

    // ---------------------------------------------------------------- the checklist reply

    #[test]
    fn parses_the_checklist_shape() {
        let raw = r#"{"owner": {"quote": "Someone needs to pull the usage report.", "question": "Who will pull it?"},
                      "date": null,
                      "conflict": {"quote": "sixty thousand", "earlier": "forty thousand", "question": "Forty or sixty?"},
                      "term": null}"#;
        let parsed = parse_candidates(raw);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].question.kind, AmbiguityKind::UnassignedAction);
        assert_eq!(
            parsed[0].question.about,
            "Someone needs to pull the usage report."
        );
        assert_eq!(parsed[1].question.kind, AmbiguityKind::Contradiction);
        assert_eq!(parsed[1].earlier.as_deref(), Some("forty thousand"));
    }

    #[test]
    fn an_all_null_checklist_is_silence() {
        let raw = r#"{"owner": null, "date": null, "conflict": null, "term": null}"#;
        assert!(parse_questions(raw, 0).is_empty());
        // A slot holding something other than a question is not one either.
        assert!(parse_questions(r#"{"owner": "none", "date": {"question": ""}}"#, 0).is_empty());
    }

    /// The whole path with the model's reply fixed: parsed, grounded, checked, capped.
    #[tokio::test]
    async fn suggested_questions_are_grounded_in_the_window() {
        #[derive(Debug)]
        struct Scripted(&'static str);

        #[async_trait::async_trait]
        impl AiBackend for Scripted {
            fn model_id(&self) -> &str {
                "scripted"
            }
            fn is_local(&self) -> bool {
                true
            }
            async fn summarize(
                &self,
                input: &crate::TranscriptInput,
            ) -> Result<crate::SummaryOutput> {
                MockBackend::new().summarize(input).await
            }
            async fn extract_decisions(
                &self,
                input: &crate::TranscriptInput,
            ) -> Result<Vec<crate::ExtractedDecision>> {
                MockBackend::new().extract_decisions(input).await
            }
            async fn extract_action_items(
                &self,
                input: &crate::TranscriptInput,
            ) -> Result<Vec<crate::ExtractedActionItem>> {
                MockBackend::new().extract_action_items(input).await
            }
            async fn chat(&self, _: &ChatRequest) -> Result<crate::ChatResponse> {
                Ok(crate::ChatResponse {
                    text: self.0.into(),
                    model: "scripted".into(),
                })
            }
        }

        let window = meeting(&[
            ("Ravi", "QA is on Monday the 13th, and Ines owns that."),
            (
                "Marcus",
                "Someone needs to pull the usage report from the admin console.",
            ),
        ]);
        let backend = Scripted(
            r#"Here you go: {"owner": {"quote": "Someone needs to pull the usage report from the admin console.",
                "question": "Who will pull the usage report?"},
              "date": {"quote": "QA is on Monday the 13th", "question": "When is QA?"},
              "conflict": null,
              "term": {"quote": "the weekly sync", "question": "What is the weekly sync?"}}"#,
        );

        let questions = suggest_questions(&backend, &window).await.unwrap();
        assert_eq!(questions.len(), 1, "{questions:?}");
        assert_eq!(questions[0].question, "Who will pull the usage report?");
        assert_eq!(
            questions[0].at_ms, 13_000,
            "the statement's time, not the poll's"
        );
    }
}
