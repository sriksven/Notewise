//! Speaker separation.
//!
//! Assigns speaker labels to transcript segments. Deliberately a **separate crate** from
//! `transcription`: speaker quality needs to be iterated on independently, and a user must be
//! able to turn it off, without touching the transcription path.
//!
//! # What is implemented here
//!
//! [`SingleSpeakerDiarizer`] is **the default**, and it attempts no separation at all. That
//! reads like a placeholder and is not one: see its docs for why timing-only separation was
//! removed from the default path after it labelled one person's pause as a second participant.
//!
//! [`PauseDiarizer`] is a real, complete algorithm that infers turn-taking from the gaps
//! between segments. It needs no model and no audio — only segment timings — so it works on
//! every platform and on imported transcripts. It is opt-in, because gaps are evidence about
//! *pacing* and it uses them as evidence about *people*: two speakers alternating cleanly are
//! separated well, one speaker thinking mid-sentence is split in two. See
//! [`PauseDiarizer::confidence`] for how to tell when its output is unreliable.
//!
//! Separating voices needs the voices. [`SpeakerEmbedder`] and [`cluster`] are the acoustic
//! half of that, already built and tested; what is missing is a [`Diarizer`] that carries
//! per-segment audio through to them. Until that exists, no caller is told speakers were
//! separated when they were not.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod cluster;
pub mod embedding;
pub mod spans;
pub mod voice;

pub use cluster::{cosine_distance, normalize, ClusterConfig};
pub use embedding::{SpeakerEmbedder, MIN_EMBEDDING_MS};
pub use spans::{samples_for, select_spans, subtract_overlaps, Span, SpanConfig};
pub use voice::{AudioDiarizer, EmbeddingDiarizer};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use notewise_transcription::{Segment, Transcript};

#[derive(Debug, Error)]
pub enum DiarizationError {
    /// This build cannot do model-based diarization.
    #[error("model-based diarization is unavailable: {reason}")]
    Unavailable { reason: &'static str },

    /// Loading or running the model failed.
    #[error("speaker model: {0}")]
    Model(String),

    /// Too little audio to identify anyone from.
    #[error("{got_ms} ms of audio is too short to identify a speaker; {needed_ms} ms is needed")]
    TooShort { got_ms: i64, needed_ms: i64 },
    #[error("speaker limit must be at least 1")]
    InvalidSpeakerLimit,

    #[error("transcript segments must be in chronological order")]
    UnorderedSegments,
}

pub type Result<T> = std::result::Result<T, DiarizationError>;

/// Assigns speaker labels to a transcript.
pub trait Diarizer: std::fmt::Debug {
    fn name(&self) -> &str;

    /// Label a transcript's segments, returning a new transcript.
    fn diarize(&self, transcript: &Transcript) -> Result<Transcript>;

    /// How much to trust this labelling, in `0.0..=1.0`.
    ///
    /// Surfaced in the UI so a user sees "speakers are a guess" rather than assuming the
    /// labels are authoritative.
    fn confidence(&self, transcript: &Transcript) -> f32;
}

/// Settings for [`PauseDiarizer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PauseConfig {
    /// A gap at least this long is read as a speaker change.
    ///
    /// 700 ms sits above a normal within-sentence breath and below a typical hand-off pause.
    /// Too low and one person's pauses fragment into several speakers; too high and a whole
    /// conversation collapses into one.
    pub turn_gap_ms: i64,

    /// Maximum distinct speakers.
    ///
    /// Bounded because the heuristic has no way to recognize a returning voice — without a
    /// cap, a long meeting accumulates a new "speaker" at every pause.
    pub max_speakers: usize,
}

impl Default for PauseConfig {
    fn default() -> Self {
        Self {
            turn_gap_ms: 700,
            max_speakers: 6,
        }
    }
}

/// Infers turn-taking from the pauses between segments.
#[derive(Debug, Clone, Default)]
pub struct PauseDiarizer {
    config: PauseConfig,
}

impl PauseDiarizer {
    pub fn new(config: PauseConfig) -> Result<Self> {
        if config.max_speakers == 0 {
            return Err(DiarizationError::InvalidSpeakerLimit);
        }
        Ok(Self { config })
    }

    pub fn config(&self) -> PauseConfig {
        self.config
    }

    fn label(index: usize) -> String {
        format!("Speaker {}", index + 1)
    }

    /// Gaps between consecutive segments.
    fn gaps(transcript: &Transcript) -> Vec<i64> {
        transcript
            .segments
            .windows(2)
            .map(|pair| (pair[1].start_ms - pair[0].end_ms).max(0))
            .collect()
    }
}

impl Diarizer for PauseDiarizer {
    fn name(&self) -> &str {
        "pause-heuristic"
    }

    fn diarize(&self, transcript: &Transcript) -> Result<Transcript> {
        if transcript.is_empty() {
            return Ok(transcript.clone());
        }

        // Out-of-order segments would make gaps meaningless and silently produce nonsense.
        if transcript
            .segments
            .windows(2)
            .any(|pair| pair[1].start_ms < pair[0].start_ms)
        {
            return Err(DiarizationError::UnorderedSegments);
        }

        let mut speaker = 0usize;
        let mut labelled: Vec<Segment> = Vec::with_capacity(transcript.segments.len());

        for (index, segment) in transcript.segments.iter().enumerate() {
            if index > 0 {
                let previous = &transcript.segments[index - 1];
                let gap = (segment.start_ms - previous.end_ms).max(0);

                if gap >= self.config.turn_gap_ms {
                    // Cycle rather than grow without bound: with two speakers this
                    // alternates correctly, which is the common case.
                    speaker = (speaker + 1) % self.config.max_speakers;
                }
            }

            let mut segment = segment.clone();
            segment.speaker = Some(Self::label(speaker));
            labelled.push(segment);
        }

        Ok(Transcript::new(labelled))
    }

    fn confidence(&self, transcript: &Transcript) -> f32 {
        if transcript.segments.len() < 2 {
            // Nothing to separate — the single label is trivially right.
            return 1.0;
        }

        let gaps = Self::gaps(transcript);
        let threshold = self.config.turn_gap_ms;

        // Confidence is how decisively gaps fall on one side of the threshold. Gaps clustered
        // near it mean the boundary is doing arbitrary work, and the labels are guesses.
        let decisive = gaps
            .iter()
            .filter(|gap| {
                let distance = (*gap - threshold).abs();
                distance > threshold / 2
            })
            .count();

        (decisive as f32 / gaps.len() as f32).clamp(0.0, 1.0)
    }
}

/// A diarizer that labels nothing.
///
/// The "speaker separation off" setting, as a type rather than a branch scattered through
/// the pipeline.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopDiarizer;

impl Diarizer for NoopDiarizer {
    fn name(&self) -> &str {
        "disabled"
    }

    fn diarize(&self, transcript: &Transcript) -> Result<Transcript> {
        Ok(transcript.clone())
    }

    fn confidence(&self, _transcript: &Transcript) -> f32 {
        1.0
    }
}

/// Labels every segment as one speaker.
///
/// # Why this is the default
///
/// Timing cannot tell a pause from a person. [`PauseDiarizer`] reads a gap as a change of
/// speaker, which is right for two people alternating cleanly and wrong for everything else —
/// including the common case of one person pausing to think, who comes out of it labelled as a
/// room full of people taking turns.
///
/// That is not a threshold that needs tuning; it is the wrong kind of evidence. A recording of
/// one person saying "hello how are you", followed by silence, produced a confident
/// `Speaker 1` / `Speaker 2` transcript here — and a user who can see there was only one
/// person in the room now has a reason to distrust every label in the app.
///
/// So this claims what the available evidence supports and nothing more: the words, in order,
/// attributed to a single voice. Separating real voices needs the voices — see
/// [`SpeakerEmbedder`], which does the acoustic half of that job and is waiting on a
/// [`Diarizer`] to cluster per-segment audio with it.
#[derive(Debug, Clone, Copy, Default)]
pub struct SingleSpeakerDiarizer;

impl SingleSpeakerDiarizer {
    /// The label every segment receives.
    pub const LABEL: &'static str = "Speaker 1";
}

impl Diarizer for SingleSpeakerDiarizer {
    fn name(&self) -> &str {
        "single-speaker"
    }

    fn diarize(&self, transcript: &Transcript) -> Result<Transcript> {
        Ok(Transcript::new(
            transcript
                .segments
                .iter()
                .map(|segment| {
                    let mut segment = segment.clone();
                    segment.speaker = Some(Self::LABEL.to_string());
                    segment
                })
                .collect(),
        ))
    }

    /// Zero, and deliberately.
    ///
    /// Not "certain there was one speaker" — no separation was attempted, so a caller asking
    /// how much to trust the *separation* must not be told to trust it. A UI reading this
    /// should offer to split speakers, not present these labels as findings.
    fn confidence(&self, _transcript: &Transcript) -> f32 {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a transcript from `(start_ms, end_ms)` pairs.
    fn transcript(spans: &[(i64, i64)]) -> Transcript {
        Transcript::new(
            spans
                .iter()
                .enumerate()
                .map(|(i, (start, end))| Segment::new(format!("line {i}"), *start, *end))
                .collect(),
        )
    }

    fn speakers(transcript: &Transcript) -> Vec<String> {
        transcript
            .segments
            .iter()
            .map(|s| s.speaker.clone().unwrap_or_default())
            .collect()
    }

    #[test]
    fn a_continuous_monologue_is_one_speaker() {
        // Short gaps throughout: one person talking without interruption.
        let input = transcript(&[(0, 1000), (1100, 2000), (2100, 3000), (3100, 4000)]);
        let output = PauseDiarizer::default().diarize(&input).unwrap();

        assert_eq!(
            speakers(&output),
            vec!["Speaker 1"; 4],
            "small pauses must not fragment one person into several"
        );
    }

    #[test]
    fn a_clean_two_person_exchange_alternates() {
        // Long gaps between each turn.
        let input = transcript(&[(0, 1000), (2000, 3000), (4000, 5000), (6000, 7000)]);
        let output = PauseDiarizer::default().diarize(&input).unwrap();

        assert_eq!(
            speakers(&output),
            vec!["Speaker 1", "Speaker 2", "Speaker 3", "Speaker 4"],
        );
    }

    #[test]
    fn mixed_pacing_groups_runs_of_speech() {
        // Two turns, each made of several closely-spaced segments.
        let input = transcript(&[
            (0, 1000),
            (1050, 2000), // same speaker continuing
            (3500, 4500), // long gap: new speaker
            (4550, 5500), // continuing
        ]);
        let output = PauseDiarizer::default().diarize(&input).unwrap();

        assert_eq!(
            speakers(&output),
            vec!["Speaker 1", "Speaker 1", "Speaker 2", "Speaker 2"],
        );
    }

    #[test]
    fn speaker_count_is_capped() {
        // Without a cap, every pause in a long meeting invents another speaker.
        let spans: Vec<(i64, i64)> = (0..20).map(|i| (i * 5000, i * 5000 + 1000)).collect();
        let diarizer = PauseDiarizer::new(PauseConfig {
            turn_gap_ms: 700,
            max_speakers: 3,
        })
        .unwrap();

        let output = diarizer.diarize(&transcript(&spans)).unwrap();
        let distinct: std::collections::HashSet<_> = speakers(&output).into_iter().collect();

        assert_eq!(distinct.len(), 3);
    }

    #[test]
    fn a_zero_speaker_limit_is_rejected() {
        assert!(matches!(
            PauseDiarizer::new(PauseConfig {
                turn_gap_ms: 700,
                max_speakers: 0,
            })
            .unwrap_err(),
            DiarizationError::InvalidSpeakerLimit
        ));
    }

    #[test]
    fn the_gap_threshold_changes_the_grouping() {
        let input = transcript(&[(0, 1000), (1500, 2500)]);

        let sensitive = PauseDiarizer::new(PauseConfig {
            turn_gap_ms: 300,
            max_speakers: 6,
        })
        .unwrap();
        let tolerant = PauseDiarizer::new(PauseConfig {
            turn_gap_ms: 2000,
            max_speakers: 6,
        })
        .unwrap();

        assert_eq!(
            speakers(&sensitive.diarize(&input).unwrap()),
            vec!["Speaker 1", "Speaker 2"]
        );
        assert_eq!(
            speakers(&tolerant.diarize(&input).unwrap()),
            vec!["Speaker 1", "Speaker 1"]
        );
    }

    #[test]
    fn an_empty_transcript_is_returned_unchanged() {
        let output = PauseDiarizer::default()
            .diarize(&Transcript::default())
            .unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn a_single_segment_gets_the_first_label() {
        let output = PauseDiarizer::default()
            .diarize(&transcript(&[(0, 1000)]))
            .unwrap();
        assert_eq!(speakers(&output), vec!["Speaker 1"]);
    }

    #[test]
    fn out_of_order_segments_are_rejected_rather_than_mislabelled() {
        // Gaps are meaningless on unordered input; producing labels anyway would be
        // confidently wrong.
        let unordered = Transcript::new(vec![
            Segment::new("second", 5000, 6000),
            Segment::new("first", 0, 1000),
        ]);

        assert!(matches!(
            PauseDiarizer::default().diarize(&unordered).unwrap_err(),
            DiarizationError::UnorderedSegments
        ));
    }

    #[test]
    fn overlapping_segments_do_not_produce_negative_gaps() {
        // Cross-talk produces overlaps; a negative gap must not wrap around.
        let overlapping = Transcript::new(vec![
            Segment::new("a", 0, 2000),
            Segment::new("b", 1000, 3000),
        ]);

        let output = PauseDiarizer::default().diarize(&overlapping).unwrap();
        assert_eq!(speakers(&output), vec!["Speaker 1", "Speaker 1"]);
    }

    #[test]
    fn diarization_preserves_text_and_timings() {
        let input = transcript(&[(0, 1000), (2000, 3000)]);
        let output = PauseDiarizer::default().diarize(&input).unwrap();

        for (before, after) in input.segments.iter().zip(&output.segments) {
            assert_eq!(before.text, after.text);
            assert_eq!(before.start_ms, after.start_ms);
            assert_eq!(before.end_ms, after.end_ms);
        }
    }

    #[test]
    fn decisive_gaps_yield_high_confidence() {
        // Every gap far from the threshold: the labelling is well-founded.
        let clear = transcript(&[(0, 1000), (1020, 2000), (8000, 9000), (8020, 10_000)]);
        assert!(
            PauseDiarizer::default().confidence(&clear) > 0.9,
            "got {}",
            PauseDiarizer::default().confidence(&clear)
        );
    }

    #[test]
    fn ambiguous_gaps_yield_low_confidence() {
        // Every gap sits right at the 700ms threshold — the boundary is doing arbitrary
        // work, and the user should be told the labels are a guess.
        let ambiguous = transcript(&[(0, 1000), (1700, 2700), (3400, 4400), (5100, 6100)]);
        assert!(
            PauseDiarizer::default().confidence(&ambiguous) < 0.3,
            "got {}",
            PauseDiarizer::default().confidence(&ambiguous)
        );
    }

    #[test]
    fn a_trivial_transcript_is_fully_confident() {
        assert_eq!(
            PauseDiarizer::default().confidence(&Transcript::default()),
            1.0
        );
        assert_eq!(
            PauseDiarizer::default().confidence(&transcript(&[(0, 1000)])),
            1.0
        );
    }

    #[test]
    fn the_noop_diarizer_leaves_segments_unlabelled() {
        let input = transcript(&[(0, 1000), (5000, 6000)]);
        let output = NoopDiarizer.diarize(&input).unwrap();

        assert!(output.segments.iter().all(|s| s.speaker.is_none()));
        assert_eq!(output, input);
    }

    #[test]
    fn diarizers_are_usable_behind_a_trait_object() {
        let diarizers: Vec<Box<dyn Diarizer>> = vec![
            Box::new(PauseDiarizer::default()),
            Box::new(NoopDiarizer),
            Box::new(SingleSpeakerDiarizer),
        ];
        let input = transcript(&[(0, 1000), (5000, 6000)]);

        for diarizer in &diarizers {
            assert!(diarizer.diarize(&input).is_ok());
            assert!((0.0..=1.0).contains(&diarizer.confidence(&input)));
        }
    }

    /// The behaviour the pipeline now depends on: silence, however long, is not a person.
    ///
    /// The spans are the real ones from the recording that motivated this — a phrase, an
    /// eight-second hole, then four more segments.
    #[test]
    fn a_single_speaker_survives_any_gap() {
        let input = transcript(&[
            (0, 2_000),
            (10_000, 11_000),
            (11_000, 12_000),
            (12_000, 13_000),
            (13_000, 14_000),
        ]);

        let output = SingleSpeakerDiarizer.diarize(&input).unwrap();

        assert_eq!(speakers(&output), vec!["Speaker 1"; 5]);
        assert_eq!(
            PauseDiarizer::default()
                .diarize(&input)
                .unwrap()
                .segments
                .iter()
                .filter_map(|s| s.speaker.clone())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            2,
            "the heuristic this replaced should still behave as documented — it splits here, \
             which is exactly why it is no longer the default"
        );
    }

    /// Everything gets a label. "Unattributed" in a transcript reads as a failure, and the
    /// words are not in doubt even when the speaker is.
    #[test]
    fn single_speaker_labels_every_segment_and_changes_nothing_else() {
        let input = transcript(&[(0, 1_000), (60_000, 61_000)]);
        let output = SingleSpeakerDiarizer.diarize(&input).unwrap();

        assert!(output.segments.iter().all(|s| s.speaker.is_some()));
        for (before, after) in input.segments.iter().zip(output.segments.iter()) {
            assert_eq!(before.text, after.text);
            assert_eq!(
                (before.start_ms, before.end_ms),
                (after.start_ms, after.end_ms)
            );
        }
    }

    /// It must not claim confidence in a separation it never attempted.
    #[test]
    fn single_speaker_claims_no_confidence_in_separation() {
        let input = transcript(&[(0, 1_000), (5_000, 6_000)]);
        assert_eq!(SingleSpeakerDiarizer.confidence(&input), 0.0);
    }

    #[test]
    fn single_speaker_handles_an_empty_transcript() {
        let output = SingleSpeakerDiarizer
            .diarize(&Transcript::default())
            .unwrap();
        assert!(output.is_empty());
    }
}
