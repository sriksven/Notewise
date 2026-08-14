//! Measuring transcription and speaker attribution against ground truth.
//!
//! # Why this crate exists
//!
//! Every change to the transcription path so far has shipped on unit tests over synthetic
//! audio, which prove the plumbing and say nothing about quality. When a real recording came
//! back with four invented `Okay.` segments and one speaker split into two, nothing in CI
//! noticed, because nothing in CI was looking at *output* — only at whether the code ran.
//!
//! Tuning a VAD threshold, swapping a decode window, changing a diarizer: each of these is a
//! quality tradeoff, and without numbers the only way to evaluate one is to record something
//! and squint at it. This crate turns that into a measurement.
//!
//! # What it measures
//!
//! - [`wer`] — Word Error Rate, the standard transcription metric: edit distance over words,
//!   normalised by reference length.
//! - [`der`] — Diarization Error Rate, the standard speaker-attribution metric: the fraction
//!   of speech time given to the wrong speaker, missed, or invented.
//!
//! Both are implemented from their public definitions (NIST rich-transcription scoring, and
//! the frame-based convention used by `pyannote` and `dscore`). No reference implementation
//! was copied.
//!
//! # Status
//!
//! The metrics are complete and tested. What this crate deliberately does *not* do yet is ship
//! a corpus: scoring needs audio with human transcripts, which has to be chosen and licensed
//! rather than invented. [`Reference`] is the shape that corpus loads into.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// A span of speech attributed to one speaker.
///
/// The unit both metrics work in, and what a ground-truth file parses into.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerSpan {
    pub speaker: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

impl SpeakerSpan {
    pub fn new(speaker: impl Into<String>, start_ms: i64, end_ms: i64) -> Self {
        Self {
            speaker: speaker.into(),
            start_ms,
            end_ms,
        }
    }

    pub fn duration_ms(&self) -> i64 {
        (self.end_ms - self.start_ms).max(0)
    }
}

/// Ground truth for one recording.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    /// What was actually said, as a human transcribed it.
    pub text: String,
    /// Who was speaking when.
    pub spans: Vec<SpeakerSpan>,
}

// ---------------------------------------------------------------- word error rate

/// How a transcript differed from the reference, word by word.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct WordErrors {
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
    /// Words in the reference. The denominator.
    pub reference_words: usize,
}

impl WordErrors {
    /// Word Error Rate: `(S + D + I) / N`.
    ///
    /// Can exceed 1.0 — a decoder that invents more words than were spoken is more than 100%
    /// wrong, and clamping that would hide exactly the failure this exists to catch. The four
    /// invented `Okay.`s over silence are that failure.
    pub fn rate(&self) -> f64 {
        if self.reference_words == 0 {
            // Nothing was said. Any word produced is an insertion, and a rate is undefined —
            // report 0.0 for a correctly empty transcript and 1.0 per invented word.
            return if self.insertions == 0 {
                0.0
            } else {
                self.insertions as f64
            };
        }
        (self.substitutions + self.deletions + self.insertions) as f64 / self.reference_words as f64
    }

    pub fn total(&self) -> usize {
        self.substitutions + self.deletions + self.insertions
    }
}

/// Score a transcript against a reference.
///
/// Comparison is over normalised words — case folded, surrounding punctuation stripped —
/// because a transcript that differs only in whether it wrote "Friday." or "friday" is not
/// wrong in any way a user cares about, and counting it as an error would drown the real
/// errors in noise.
pub fn wer(reference: &str, hypothesis: &str) -> WordErrors {
    let reference: Vec<String> = normalize_words(reference);
    let hypothesis: Vec<String> = normalize_words(hypothesis);

    // Levenshtein over words, tracking which operation each step took. Two rows rather than
    // the full matrix would be enough for the distance, but not for the S/D/I split, which is
    // the part that says *how* a change made things worse.
    let (rows, cols) = (reference.len() + 1, hypothesis.len() + 1);
    let mut cost = vec![vec![0usize; cols]; rows];
    let mut ops = vec![vec![Op::Match; cols]; rows];

    for (i, row) in cost.iter_mut().enumerate().take(rows) {
        row[0] = i;
        ops[i][0] = Op::Deletion;
    }
    for j in 0..cols {
        cost[0][j] = j;
        ops[0][j] = Op::Insertion;
    }
    ops[0][0] = Op::Match;

    for i in 1..rows {
        for j in 1..cols {
            if reference[i - 1] == hypothesis[j - 1] {
                cost[i][j] = cost[i - 1][j - 1];
                ops[i][j] = Op::Match;
                continue;
            }

            let substitute = cost[i - 1][j - 1] + 1;
            let delete = cost[i - 1][j] + 1;
            let insert = cost[i][j - 1] + 1;

            let best = substitute.min(delete).min(insert);
            cost[i][j] = best;
            ops[i][j] = if best == substitute {
                Op::Substitution
            } else if best == delete {
                Op::Deletion
            } else {
                Op::Insertion
            };
        }
    }

    let mut errors = WordErrors {
        substitutions: 0,
        deletions: 0,
        insertions: 0,
        reference_words: reference.len(),
    };

    let (mut i, mut j) = (reference.len(), hypothesis.len());
    while i > 0 || j > 0 {
        match ops[i][j] {
            Op::Match => {
                i -= 1;
                j -= 1;
            }
            Op::Substitution => {
                errors.substitutions += 1;
                i -= 1;
                j -= 1;
            }
            Op::Deletion => {
                errors.deletions += 1;
                i -= 1;
            }
            Op::Insertion => {
                errors.insertions += 1;
                j -= 1;
            }
        }
    }

    errors
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Match,
    Substitution,
    Deletion,
    Insertion,
}

/// Split into comparable words: case folded, outer punctuation removed.
fn normalize_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

// ---------------------------------------------------------------- diarization error rate

/// How speaker attribution differed from the reference.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SpeakerErrors {
    /// Speech given to the wrong speaker, in milliseconds.
    pub confusion_ms: i64,
    /// Speech the system did not label at all, in milliseconds.
    pub missed_ms: i64,
    /// Silence the system labelled as somebody talking, in milliseconds.
    pub false_alarm_ms: i64,
    /// Reference speech. The denominator.
    pub speech_ms: i64,
}

impl SpeakerErrors {
    /// Diarization Error Rate: `(confusion + missed + false alarm) / total speech`.
    pub fn rate(&self) -> f64 {
        if self.speech_ms == 0 {
            return if self.false_alarm_ms == 0 { 0.0 } else { 1.0 };
        }
        (self.confusion_ms + self.missed_ms + self.false_alarm_ms) as f64 / self.speech_ms as f64
    }
}

/// Frame size for the timeline, in milliseconds. 10 ms is the scoring convention.
const FRAME_MS: i64 = 10;

/// Score speaker attribution against a reference.
///
/// # Label matching
///
/// A diarizer's labels are arbitrary — its "Speaker 1" and the reference's "Alex" refer to the
/// same person only by coincidence of ordering. Before scoring, hypothesis labels are mapped
/// onto reference labels by maximum overlap, greedily and one-to-one: the pair sharing the most
/// speech is matched first, then the next, and so on.
///
/// Greedy rather than optimal (Hungarian) assignment. Where two hypothesis clusters compete for
/// one reference speaker, the loser stays unmapped and its frames score as confusion — which
/// overstates the error slightly, in the direction of not flattering the system under test.
pub fn der(reference: &[SpeakerSpan], hypothesis: &[SpeakerSpan]) -> SpeakerErrors {
    let end_ms = reference
        .iter()
        .chain(hypothesis.iter())
        .map(|s| s.end_ms)
        .max()
        .unwrap_or(0);

    let frames = (end_ms / FRAME_MS).max(0) as usize;
    let reference_frames = paint(reference, frames);
    let hypothesis_frames = paint(hypothesis, frames);

    let mapping = match_labels(&reference_frames, &hypothesis_frames);

    let mut errors = SpeakerErrors {
        confusion_ms: 0,
        missed_ms: 0,
        false_alarm_ms: 0,
        speech_ms: 0,
    };

    for frame in 0..frames {
        let truth = reference_frames[frame].as_deref();
        let labelled = hypothesis_frames[frame].as_deref();
        // An unmapped label is still a label. The system claimed someone was talking here; it
        // just named a speaker with no counterpart in the reference. That is a speaker error,
        // not a missed detection — "missed" is reserved for saying nothing at all.
        let mapped = labelled.and_then(|label| mapping.get(label).map(String::as_str));

        if truth.is_some() {
            errors.speech_ms += FRAME_MS;
        }

        match (truth, labelled) {
            (None, None) => {}
            // Labelled a stretch nobody was talking in.
            (None, Some(_)) => errors.false_alarm_ms += FRAME_MS,
            // Someone was talking and the system said nothing.
            (Some(_), None) => errors.missed_ms += FRAME_MS,
            (Some(truth), Some(_)) => {
                if mapped != Some(truth) {
                    errors.confusion_ms += FRAME_MS;
                }
            }
        }
    }

    errors
}

/// Lay spans onto a frame timeline, one label per frame.
///
/// Where spans overlap the later one wins. Overlapping speech is a real thing this cannot
/// represent; scoring it properly needs an overlap-aware reference, and pretending otherwise
/// would be worse than the documented simplification.
fn paint(spans: &[SpeakerSpan], frames: usize) -> Vec<Option<String>> {
    let mut timeline = vec![None; frames];

    for span in spans {
        let start = (span.start_ms.max(0) / FRAME_MS) as usize;
        let end = ((span.end_ms.max(0) + FRAME_MS - 1) / FRAME_MS) as usize;

        for frame in timeline.iter_mut().take(end.min(frames)).skip(start) {
            *frame = Some(span.speaker.clone());
        }
    }

    timeline
}

/// Map hypothesis labels onto reference labels by greatest overlap.
fn match_labels(
    reference: &[Option<String>],
    hypothesis: &[Option<String>],
) -> HashMap<String, String> {
    let mut overlap: HashMap<(&str, &str), usize> = HashMap::new();

    for (truth, guess) in reference.iter().zip(hypothesis.iter()) {
        if let (Some(truth), Some(guess)) = (truth, guess) {
            *overlap.entry((guess.as_str(), truth.as_str())).or_insert(0) += 1;
        }
    }

    // Most overlap first, with the label pair as a tiebreak so the result does not depend on
    // HashMap iteration order.
    let mut pairs: Vec<((&str, &str), usize)> = overlap.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut mapping = HashMap::new();
    let mut claimed: HashSet<&str> = HashSet::new();

    for ((guess, truth), _) in pairs {
        if mapping.contains_key(guess) || claimed.contains(truth) {
            continue;
        }
        mapping.insert(guess.to_string(), truth.to_string());
        claimed.insert(truth);
    }

    mapping
}

/// Turn stored transcript segments into spans for scoring.
pub fn spans_from_segments(segments: &[notewise_transcription::Segment]) -> Vec<SpeakerSpan> {
    segments
        .iter()
        .filter_map(|segment| {
            segment
                .speaker
                .as_ref()
                .map(|speaker| SpeakerSpan::new(speaker, segment.start_ms, segment.end_ms))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------ word error rate

    #[test]
    fn a_perfect_transcript_has_no_errors() {
        let errors = wer("we ship on Friday", "We ship on Friday.");
        assert_eq!(errors.total(), 0);
        assert_eq!(errors.rate(), 0.0);
    }

    #[test]
    fn punctuation_and_case_are_not_errors() {
        // A transcript differing only in these is not wrong in any way a user cares about.
        let errors = wer(
            "Postgres migration lands Friday",
            "postgres, migration lands friday!",
        );
        assert_eq!(errors.total(), 0);
    }

    #[test]
    fn the_three_error_kinds_are_counted_separately() {
        // Reference: a b c d
        // Hypothesis: a x c d e   -> one substitution (b→x), one insertion (e)
        let errors = wer("a b c d", "a x c d e");
        assert_eq!(errors.substitutions, 1);
        assert_eq!(errors.insertions, 1);
        assert_eq!(errors.deletions, 0);
        assert_eq!(errors.reference_words, 4);
        assert_eq!(errors.rate(), 0.5);
    }

    #[test]
    fn dropped_words_are_deletions() {
        let errors = wer("we ship on Friday", "we ship Friday");
        assert_eq!(errors.deletions, 1);
        assert_eq!(errors.substitutions, 0);
        assert_eq!(errors.insertions, 0);
    }

    /// The metric has to catch the bug that motivated this crate: text invented over silence.
    #[test]
    fn words_invented_over_silence_score_as_error_not_as_success() {
        let errors = wer("", "Okay. Okay. Okay. Okay.");

        assert_eq!(errors.insertions, 4);
        assert_eq!(errors.deletions, 0);
        assert!(
            errors.rate() > 0.0,
            "a transcript of a silent recording that contains four words must not score clean"
        );
    }

    #[test]
    fn a_correctly_empty_transcript_is_not_an_error() {
        assert_eq!(wer("", "").rate(), 0.0);
    }

    #[test]
    fn a_completely_wrong_transcript_can_exceed_one() {
        // Clamping this would hide a decoder that invents more than it transcribes.
        let errors = wer("yes", "no it is not at all like that");
        assert!(errors.rate() > 1.0, "rate was {}", errors.rate());
    }

    // ------------------------------------------------------------ diarization error rate

    #[test]
    fn identical_attribution_scores_zero() {
        let truth = vec![
            SpeakerSpan::new("Alex", 0, 2_000),
            SpeakerSpan::new("Sam", 2_000, 4_000),
        ];
        assert_eq!(der(&truth, &truth).rate(), 0.0);
    }

    /// Labels are arbitrary. Getting the *separation* right with different names is a perfect
    /// score, and a scorer that missed this would reject every correct diarizer.
    #[test]
    fn label_names_do_not_matter_only_the_separation() {
        let truth = vec![
            SpeakerSpan::new("Alex", 0, 2_000),
            SpeakerSpan::new("Sam", 2_000, 4_000),
        ];
        let guess = vec![
            SpeakerSpan::new("Speaker 1", 0, 2_000),
            SpeakerSpan::new("Speaker 2", 2_000, 4_000),
        ];

        assert_eq!(der(&truth, &guess).rate(), 0.0);
    }

    /// The phantom-speaker bug, scored: one person, split in two by the old heuristic.
    #[test]
    fn splitting_one_speaker_into_two_is_penalised() {
        let truth = vec![SpeakerSpan::new("Alex", 0, 4_000)];
        let guess = vec![
            SpeakerSpan::new("Speaker 1", 0, 2_000),
            SpeakerSpan::new("Speaker 2", 2_000, 4_000),
        ];

        let errors = der(&truth, &guess);
        // Half the speech went to a person who was not there. Only one hypothesis label can
        // map to Alex; the other's frames are confusion.
        assert_eq!(errors.confusion_ms, 2_000);
        assert_eq!(errors.speech_ms, 4_000);
        assert_eq!(errors.rate(), 0.5);
    }

    /// The opposite failure: two real people collapsed into one label, which is what the
    /// single-speaker default does on a genuine two-person meeting.
    #[test]
    fn merging_two_speakers_into_one_is_penalised() {
        let truth = vec![
            SpeakerSpan::new("Alex", 0, 2_000),
            SpeakerSpan::new("Sam", 2_000, 4_000),
        ];
        let guess = vec![SpeakerSpan::new("Speaker 1", 0, 4_000)];

        let errors = der(&truth, &guess);
        assert_eq!(errors.confusion_ms, 2_000);
        assert_eq!(errors.rate(), 0.5);
    }

    #[test]
    fn unlabelled_speech_is_a_miss_and_invented_speech_is_a_false_alarm() {
        let truth = vec![SpeakerSpan::new("Alex", 0, 2_000)];

        let missed = der(&truth, &[]);
        assert_eq!(missed.missed_ms, 2_000);
        assert_eq!(missed.rate(), 1.0);

        let invented = der(&truth, &[SpeakerSpan::new("Alex", 0, 3_000)]);
        assert_eq!(invented.false_alarm_ms, 1_000);
        assert_eq!(invented.missed_ms, 0);
    }

    #[test]
    fn scoring_nothing_against_nothing_is_not_an_error() {
        assert_eq!(der(&[], &[]).rate(), 0.0);
    }

    /// Perfect channel attribution — what dual-channel capture produces — must score zero even
    /// though the two speakers interrupt each other, which is the case heuristics fail on.
    #[test]
    fn channel_attribution_of_interrupting_speakers_scores_clean() {
        let truth = vec![
            SpeakerSpan::new("You", 0, 1_500),
            SpeakerSpan::new("Others", 1_400, 3_000),
            SpeakerSpan::new("You", 3_000, 4_000),
        ];

        assert_eq!(der(&truth, &truth).rate(), 0.0);
    }

    #[test]
    fn segments_convert_to_spans_dropping_unattributed_ones() {
        let mut attributed = notewise_transcription::Segment::new("hello", 0, 1_000);
        attributed.speaker = Some("You".into());
        let unattributed = notewise_transcription::Segment::new("world", 1_000, 2_000);

        let spans = spans_from_segments(&[attributed, unattributed]);
        assert_eq!(spans, vec![SpeakerSpan::new("You", 0, 1_000)]);
    }
}
