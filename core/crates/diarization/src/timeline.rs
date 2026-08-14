//! Speaker identity, as reported by the meeting platform.
//!
//! The other two diarizers in this crate reason about evidence we derived ourselves: gaps
//! between segments ([`crate::PauseDiarizer`]) or the sound of a voice
//! ([`crate::EmbeddingDiarizer`]). This module handles evidence someone else already computed —
//! Google Meet, Zoom, and Teams all know exactly who is talking, because they are the ones
//! routing the audio, and they display it in their own UI.
//!
//! That makes this the only source in the crate that can produce a **name**. Clustering can
//! tell you there were five voices; it can never tell you one of them was Priya.
//!
//! # The seam
//!
//! [`SpeakerTimeline`] is deliberately silent on how the events were obtained. A browser
//! extension reading the participant list of the user's own meeting, a headless bot, a Zoom
//! Meeting SDK sidecar, a hosted vendor's webhook, and a human correcting labels by hand all
//! produce this same structure. Which one is in use is configuration, not architecture.
//!
//! # Two things that will cause bugs if ignored
//!
//! **Timestamps are milliseconds since the recording started, not wall clock.** The producer of
//! these events (a browser tab) and the consumer (an audio pipeline) have different clocks, and
//! the offset between them is not knowable after the fact. Whoever feeds this must convert at
//! the boundary, once, against the recording's start.
//!
//! **Turns overlap, because people talk over each other.** Nothing here assumes one speaker at
//! a time. [`SpeakerTimeline::speaker_for`] resolves an overlap by total overlapping duration,
//! which is a choice and not a fact — see its docs.

use serde::{Deserialize, Serialize};

use crate::{DiarizationError, Diarizer, Result};
use notewise_transcription::{Segment, Transcript};

/// Repeated reports of the same speaker closer than this are treated as one turn.
///
/// A dominant-speaker feed is a poll, not an edge trigger: a browser extension watching the
/// active-speaker indicator emits "still Priya" several times a second. Left alone that turns an
/// hour-long meeting into tens of thousands of adjacent turns naming the same person, which
/// costs memory and makes every overlap calculation slower for no added information.
///
/// 250 ms is above a comfortable polling interval and well below the shortest gap that means
/// anything conversationally, so coalescing here cannot merge two genuinely separate turns by
/// the same person into one in a way that changes any label.
pub const TURN_COALESCE_TOLERANCE_MS: i64 = 250;

/// A participant's identifier within one meeting.
///
/// Opaque and platform-supplied. Meet, Zoom, and Teams each have their own format and none of
/// them is a stable identity for the *person* across meetings — only within this one. Anything
/// wanting cross-meeting identity needs a `people` row, not this.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ParticipantId(String);

impl ParticipantId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Someone in the meeting, as the platform names them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    pub id: ParticipantId,
    /// What the platform displays. A human-chosen string: it may be a full name, a first name,
    /// "iPhone", or empty. Not validated, not parsed, and never treated as an identity.
    pub display_name: String,
}

impl Participant {
    pub fn new(id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            id: ParticipantId::new(id),
            display_name: display_name.into(),
        }
    }
}

/// A stretch during which the platform reported one participant as speaking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerTurn {
    pub participant: ParticipantId,
    /// Milliseconds since recording start. See the module docs on clocks.
    pub start_ms: i64,
    pub end_ms: i64,
}

impl SpeakerTurn {
    pub fn duration_ms(&self) -> i64 {
        (self.end_ms - self.start_ms).max(0)
    }

    /// Milliseconds this turn and `[start_ms, end_ms)` have in common.
    fn overlap_with(&self, start_ms: i64, end_ms: i64) -> i64 {
        (self.end_ms.min(end_ms) - self.start_ms.max(start_ms)).max(0)
    }
}

/// Who the platform says was talking, when.
///
/// Built up as events arrive during a recording, then handed to [`TimelineDiarizer`] or
/// [`crate::NamedClusterDiarizer`] when the recording stops.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerTimeline {
    participants: Vec<Participant>,
    /// Sorted by `start_ms`. Maintained on insert so consumers never have to sort, and so a
    /// malformed feed cannot produce order-dependent labels.
    turns: Vec<SpeakerTurn>,
}

impl SpeakerTimeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a participant, replacing any existing display name for the same id.
    ///
    /// Replacing rather than rejecting is deliberate: people rename themselves mid-meeting, and
    /// the platform reports a late-joining participant's name only once it has it. The most
    /// recent name the platform gave us is the best one we have.
    pub fn upsert_participant(&mut self, participant: Participant) {
        match self
            .participants
            .iter_mut()
            .find(|existing| existing.id == participant.id)
        {
            Some(existing) => existing.display_name = participant.display_name,
            None => self.participants.push(participant),
        }
    }

    /// Record that a participant was speaking over `[start_ms, end_ms)`.
    ///
    /// Coalesces with that participant's most recent turn when the two are contiguous within
    /// [`TURN_COALESCE_TOLERANCE_MS`] — see that constant for why.
    ///
    /// # Errors
    ///
    /// [`DiarizationError::UnknownParticipant`] if the id was never passed to
    /// [`Self::upsert_participant`]. A turn naming someone we cannot name is not something to
    /// store and work out later: it would either become a hole in the transcript or, worse, get
    /// filled in from a neighbour and attribute words to whoever happened to be adjacent.
    ///
    /// [`DiarizationError::InvalidTurn`] if the turn ends before it starts, or starts before the
    /// recording did. Both mean the producer's clock conversion is wrong, and a negative
    /// timestamp silently poisons every overlap calculation downstream.
    pub fn add_turn(
        &mut self,
        participant: ParticipantId,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<()> {
        if end_ms < start_ms || start_ms < 0 {
            return Err(DiarizationError::InvalidTurn { start_ms, end_ms });
        }
        if !self.participants.iter().any(|p| p.id == participant) {
            return Err(DiarizationError::UnknownParticipant {
                id: participant.to_string(),
            });
        }

        // Coalesce with this speaker's latest turn when they run together. Scanning from the end
        // finds it immediately for an in-order feed, which is every real feed.
        if let Some(previous) = self
            .turns
            .iter_mut()
            .rev()
            .find(|turn| turn.participant == participant)
        {
            if start_ms <= previous.end_ms + TURN_COALESCE_TOLERANCE_MS
                && start_ms >= previous.start_ms
            {
                previous.end_ms = previous.end_ms.max(end_ms);
                return Ok(());
            }
        }

        let turn = SpeakerTurn {
            participant,
            start_ms,
            end_ms,
        };

        // Append when already in order — the common case — and binary-search insert otherwise,
        // so an out-of-order event costs a memmove rather than a re-sort.
        match self.turns.last() {
            Some(last) if last.start_ms > start_ms => {
                let at = self.turns.partition_point(|t| t.start_ms <= start_ms);
                self.turns.insert(at, turn);
            }
            _ => self.turns.push(turn),
        }

        Ok(())
    }

    pub fn participants(&self) -> &[Participant] {
        &self.participants
    }

    /// Turns, sorted by start time.
    pub fn turns(&self) -> &[SpeakerTurn] {
        &self.turns
    }

    /// No turns to attribute anything from.
    ///
    /// A roster with no speech is empty for this purpose: knowing five people were present
    /// labels nothing.
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    pub fn name_of(&self, id: &ParticipantId) -> Option<&str> {
        self.participants
            .iter()
            .find(|p| &p.id == id)
            .map(|p| p.display_name.as_str())
    }

    /// A copy of this timeline with one participant's turns removed.
    ///
    /// # Why the local user has to come out
    ///
    /// The system audio tap records what the machine *plays*, which is everyone except the person
    /// at it — their own voice goes to the microphone on a separate channel. A platform roster does
    /// not make that distinction: it reports the local user's turns alongside everyone else's.
    ///
    /// Applied unfiltered to the system channel, those turns are a hazard. When the user speaks and
    /// something bleeds into the tap, or a remote segment merely straddles their turn, the segment
    /// is named after the one person who provably did not say it.
    ///
    /// Whoever produces the events knows which participant is local — a meeting UI marks the user's
    /// own tile — so the caller removes them before refining the remote channel.
    pub fn excluding(&self, id: &ParticipantId) -> Self {
        Self {
            participants: self
                .participants
                .iter()
                .filter(|p| &p.id != id)
                .cloned()
                .collect(),
            turns: self
                .turns
                .iter()
                .filter(|t| &t.participant != id)
                .cloned()
                .collect(),
        }
    }

    /// The participant who did the most talking during `[start_ms, end_ms)`.
    ///
    /// # Why the longest overlap, and what that costs
    ///
    /// During cross-talk several turns cover the same window. Picking the one with the most
    /// milliseconds inside it is right for the common case — one person speaking while another
    /// interjects "mm-hm" — and wrong when two people genuinely share a segment, where the
    /// quieter half of the exchange is dropped rather than split.
    ///
    /// Splitting is not available: a transcript segment is one string of text with one speaker
    /// field, so the choice is which single name to attach. This returns the better guess and
    /// [`TimelineDiarizer::confidence`] reports how much of the transcript rested on guesses.
    ///
    /// `None` when no turn overlaps at all, which is honest rather than unhelpful — see
    /// [`TimelineDiarizer::diarize`] for what it does with that.
    pub fn speaker_for(&self, start_ms: i64, end_ms: i64) -> Option<&Participant> {
        let mut best: Option<(i64, &ParticipantId)> = None;

        for turn in &self.turns {
            // Sorted by start, so once a turn begins after the window ends, so does every
            // turn after it.
            if turn.start_ms >= end_ms {
                break;
            }
            let overlap = turn.overlap_with(start_ms, end_ms);
            if overlap == 0 {
                continue;
            }
            if best.is_none_or(|(most, _)| overlap > most) {
                best = Some((overlap, &turn.participant));
            }
        }

        let (_, id) = best?;
        self.participants.iter().find(|p| &p.id == id)
    }

    /// Total milliseconds of `[start_ms, end_ms)` covered by any turn.
    ///
    /// Counts overlapping turns once. Used for confidence: a window no turn covers was labelled
    /// by something other than the platform.
    pub fn covered_ms(&self, start_ms: i64, end_ms: i64) -> i64 {
        let mut covered = 0i64;
        let mut cursor = start_ms;

        for turn in &self.turns {
            if turn.start_ms >= end_ms {
                break;
            }
            let from = turn.start_ms.max(cursor);
            let to = turn.end_ms.min(end_ms);
            if to > from {
                covered += to - from;
                cursor = to;
            }
        }

        covered
    }
}

/// Labels segments from platform-reported speaker turns.
///
/// The only diarizer here that produces real names. No model, no audio, no network: an interval
/// join between two lists.
#[derive(Debug, Clone)]
pub struct TimelineDiarizer {
    timeline: SpeakerTimeline,
}

impl TimelineDiarizer {
    pub fn new(timeline: SpeakerTimeline) -> Self {
        Self { timeline }
    }

    pub fn timeline(&self) -> &SpeakerTimeline {
        &self.timeline
    }
}

impl Diarizer for TimelineDiarizer {
    fn name(&self) -> &str {
        "platform-timeline"
    }

    /// Label each segment with whoever the platform had speaking through most of it.
    ///
    /// # Segments no turn covers are left unlabelled, deliberately
    ///
    /// [`crate::EmbeddingDiarizer`] fills a gap from the nearest cluster, which is right there:
    /// the labels are anonymous, so a wrong guess mislabels `Speaker 2` as `Speaker 1`.
    ///
    /// Here the labels are people's names. Filling a gap from a neighbour would state that a
    /// named colleague said words they did not say — in a transcript the user may forward,
    /// quote, or act on. An empty speaker field is recoverable; a plausible wrong name is not,
    /// because nothing downstream can tell it from a right one.
    ///
    /// [`crate::NamedClusterDiarizer`] is the way to fill these holes honestly: it falls back to
    /// an anonymous acoustic label rather than to a name.
    fn diarize(&self, transcript: &Transcript) -> Result<Transcript> {
        let segments: Vec<Segment> = transcript
            .segments
            .iter()
            .map(|segment| {
                let mut segment = segment.clone();
                segment.speaker = self
                    .timeline
                    .speaker_for(segment.start_ms, segment.end_ms)
                    .map(|p| p.display_name.clone());
                segment
            })
            .collect();

        Ok(Transcript::new(segments))
    }

    /// The share of transcript time the platform actually reported a speaker for.
    ///
    /// Not a measure of whether the names are right — the platform knows who was talking, so a
    /// covered segment is as good as attribution gets. It measures how much of the transcript
    /// this diarizer had any evidence about at all. A feed that dropped halfway through a
    /// meeting scores 0.5, and the user should be told the second half is unattributed.
    fn confidence(&self, transcript: &Transcript) -> f32 {
        if transcript.is_empty() {
            return 1.0;
        }

        let mut total = 0i64;
        let mut covered = 0i64;

        for segment in &transcript.segments {
            let duration = (segment.end_ms - segment.start_ms).max(0);
            total += duration;
            covered += self
                .timeline
                .covered_ms(segment.start_ms, segment.end_ms)
                .min(duration);
        }

        if total == 0 {
            // Zero-length segments carry no duration to weigh. Whether they were labelled is
            // the only signal available.
            let labelled = transcript
                .segments
                .iter()
                .filter(|s| self.timeline.speaker_for(s.start_ms, s.end_ms).is_some())
                .count();
            return labelled as f32 / transcript.segments.len() as f32;
        }

        (covered as f32 / total as f32).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(spans: &[(i64, i64)]) -> Transcript {
        Transcript::new(
            spans
                .iter()
                .enumerate()
                .map(|(i, (start, end))| Segment::new(format!("line {i}"), *start, *end))
                .collect(),
        )
    }

    fn speakers(transcript: &Transcript) -> Vec<Option<String>> {
        transcript
            .segments
            .iter()
            .map(|s| s.speaker.clone())
            .collect()
    }

    /// A timeline with two people and one turn each.
    fn two_people() -> SpeakerTimeline {
        let mut timeline = SpeakerTimeline::new();
        timeline.upsert_participant(Participant::new("p1", "Priya"));
        timeline.upsert_participant(Participant::new("p2", "Marcus"));
        timeline
            .add_turn(ParticipantId::new("p1"), 0, 5_000)
            .unwrap();
        timeline
            .add_turn(ParticipantId::new("p2"), 5_000, 10_000)
            .unwrap();
        timeline
    }

    #[test]
    fn turns_label_segments_with_real_names() {
        let diarizer = TimelineDiarizer::new(two_people());
        let output = diarizer
            .diarize(&transcript(&[(0, 2_000), (6_000, 8_000)]))
            .unwrap();

        assert_eq!(
            speakers(&output),
            vec![Some("Priya".to_string()), Some("Marcus".to_string())]
        );
    }

    /// The whole point of the feature: five people, five names, no clustering.
    #[test]
    fn five_participants_are_separated_and_named() {
        let mut timeline = SpeakerTimeline::new();
        let names = ["Priya", "Marcus", "Ana", "Jun", "Tobi"];
        for (i, name) in names.iter().enumerate() {
            timeline.upsert_participant(Participant::new(format!("p{i}"), *name));
        }
        for i in 0..5i64 {
            timeline
                .add_turn(
                    ParticipantId::new(format!("p{i}")),
                    i * 10_000,
                    i * 10_000 + 9_000,
                )
                .unwrap();
        }

        let segments: Vec<(i64, i64)> = (0..5).map(|i| (i * 10_000, i * 10_000 + 8_000)).collect();
        let output = TimelineDiarizer::new(timeline)
            .diarize(&transcript(&segments))
            .unwrap();

        assert_eq!(
            speakers(&output),
            names
                .iter()
                .map(|n| Some(n.to_string()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_segment_no_turn_covers_is_left_unlabelled_rather_than_guessed() {
        let diarizer = TimelineDiarizer::new(two_people());
        // 20s–21s: the feed reported nobody.
        let output = diarizer.diarize(&transcript(&[(20_000, 21_000)])).unwrap();

        assert_eq!(
            speakers(&output),
            vec![None],
            "a plausible wrong name is worse than no name"
        );
    }

    #[test]
    fn the_longest_overlap_wins_during_cross_talk() {
        let mut timeline = SpeakerTimeline::new();
        timeline.upsert_participant(Participant::new("p1", "Priya"));
        timeline.upsert_participant(Participant::new("p2", "Marcus"));
        // Priya holds the floor 0–4s; Marcus interjects 3.5–4s.
        timeline
            .add_turn(ParticipantId::new("p1"), 0, 4_000)
            .unwrap();
        timeline
            .add_turn(ParticipantId::new("p2"), 3_500, 4_000)
            .unwrap();

        let output = TimelineDiarizer::new(timeline)
            .diarize(&transcript(&[(0, 4_000)]))
            .unwrap();

        assert_eq!(speakers(&output), vec![Some("Priya".to_string())]);
    }

    #[test]
    fn a_turn_for_an_unknown_participant_is_rejected() {
        let mut timeline = SpeakerTimeline::new();
        timeline.upsert_participant(Participant::new("p1", "Priya"));

        assert!(matches!(
            timeline
                .add_turn(ParticipantId::new("ghost"), 0, 1_000)
                .unwrap_err(),
            DiarizationError::UnknownParticipant { .. }
        ));
    }

    #[test]
    fn a_backwards_or_negative_turn_is_rejected() {
        let mut timeline = SpeakerTimeline::new();
        timeline.upsert_participant(Participant::new("p1", "Priya"));

        assert!(matches!(
            timeline
                .add_turn(ParticipantId::new("p1"), 5_000, 1_000)
                .unwrap_err(),
            DiarizationError::InvalidTurn { .. }
        ));
        assert!(
            matches!(
                timeline
                    .add_turn(ParticipantId::new("p1"), -1_000, 1_000)
                    .unwrap_err(),
                DiarizationError::InvalidTurn { .. }
            ),
            "a negative start means the producer's clock conversion is wrong"
        );
    }

    /// A polling feed emits the same speaker repeatedly; that must not become thousands of turns.
    #[test]
    fn repeated_reports_of_one_speaker_coalesce() {
        let mut timeline = SpeakerTimeline::new();
        timeline.upsert_participant(Participant::new("p1", "Priya"));

        for i in 0..40i64 {
            timeline
                .add_turn(ParticipantId::new("p1"), i * 100, i * 100 + 100)
                .unwrap();
        }

        assert_eq!(timeline.turns().len(), 1, "one continuous turn, not forty");
        assert_eq!(timeline.turns()[0].start_ms, 0);
        assert_eq!(timeline.turns()[0].end_ms, 4_000);
    }

    #[test]
    fn a_real_gap_does_not_coalesce() {
        let mut timeline = SpeakerTimeline::new();
        timeline.upsert_participant(Participant::new("p1", "Priya"));
        timeline
            .add_turn(ParticipantId::new("p1"), 0, 1_000)
            .unwrap();
        // Well past the tolerance: Priya stopped and started again.
        timeline
            .add_turn(ParticipantId::new("p1"), 30_000, 31_000)
            .unwrap();

        assert_eq!(timeline.turns().len(), 2);
    }

    #[test]
    fn out_of_order_events_are_stored_in_order() {
        let mut timeline = SpeakerTimeline::new();
        timeline.upsert_participant(Participant::new("p1", "Priya"));
        timeline.upsert_participant(Participant::new("p2", "Marcus"));

        timeline
            .add_turn(ParticipantId::new("p1"), 10_000, 11_000)
            .unwrap();
        timeline
            .add_turn(ParticipantId::new("p2"), 2_000, 3_000)
            .unwrap();

        let starts: Vec<i64> = timeline.turns().iter().map(|t| t.start_ms).collect();
        assert_eq!(starts, vec![2_000, 10_000], "turns must stay sorted");
    }

    #[test]
    fn a_renamed_participant_keeps_one_entry_with_the_latest_name() {
        let mut timeline = SpeakerTimeline::new();
        timeline.upsert_participant(Participant::new("p1", "iPhone"));
        timeline.upsert_participant(Participant::new("p1", "Priya"));

        assert_eq!(timeline.participants().len(), 1);
        assert_eq!(timeline.name_of(&ParticipantId::new("p1")), Some("Priya"));
    }

    #[test]
    fn full_coverage_is_full_confidence() {
        let diarizer = TimelineDiarizer::new(two_people());
        let covered = transcript(&[(0, 5_000), (5_000, 10_000)]);

        assert!(
            diarizer.confidence(&covered) > 0.99,
            "got {}",
            diarizer.confidence(&covered)
        );
    }

    /// A feed that dies mid-meeting must report that, not claim the whole transcript.
    #[test]
    fn partial_coverage_is_reported_proportionally() {
        let diarizer = TimelineDiarizer::new(two_people());
        // Half the transcript sits after the last turn ends at 10s.
        let half = transcript(&[
            (0, 5_000),
            (5_000, 10_000),
            (10_000, 15_000),
            (15_000, 20_000),
        ]);

        let confidence = diarizer.confidence(&half);
        assert!(
            (0.45..=0.55).contains(&confidence),
            "expected about half, got {confidence}"
        );
    }

    #[test]
    fn no_turns_means_no_confidence_and_no_labels() {
        let mut timeline = SpeakerTimeline::new();
        timeline.upsert_participant(Participant::new("p1", "Priya"));
        assert!(
            timeline.is_empty(),
            "a roster without speech labels nothing"
        );

        let diarizer = TimelineDiarizer::new(timeline);
        let input = transcript(&[(0, 1_000)]);

        assert_eq!(diarizer.confidence(&input), 0.0);
        assert_eq!(speakers(&diarizer.diarize(&input).unwrap()), vec![None]);
    }

    #[test]
    fn overlapping_turns_do_not_count_coverage_twice() {
        let mut timeline = SpeakerTimeline::new();
        timeline.upsert_participant(Participant::new("p1", "Priya"));
        timeline.upsert_participant(Participant::new("p2", "Marcus"));
        timeline
            .add_turn(ParticipantId::new("p1"), 0, 4_000)
            .unwrap();
        timeline
            .add_turn(ParticipantId::new("p2"), 0, 4_000)
            .unwrap();

        assert_eq!(
            timeline.covered_ms(0, 4_000),
            4_000,
            "two people talking over four seconds is four seconds, not eight"
        );
    }

    #[test]
    fn an_empty_transcript_is_fully_confident_and_unchanged() {
        let diarizer = TimelineDiarizer::new(two_people());
        assert_eq!(diarizer.confidence(&Transcript::default()), 1.0);
        assert!(diarizer.diarize(&Transcript::default()).unwrap().is_empty());
    }

    #[test]
    fn labelling_preserves_text_and_timings() {
        let input = transcript(&[(0, 2_000), (6_000, 8_000)]);
        let output = TimelineDiarizer::new(two_people()).diarize(&input).unwrap();

        for (before, after) in input.segments.iter().zip(&output.segments) {
            assert_eq!(before.text, after.text);
            assert_eq!(
                (before.start_ms, before.end_ms),
                (after.start_ms, after.end_ms)
            );
        }
    }

    #[test]
    fn the_timeline_round_trips_through_serde() {
        // It crosses the local API boundary from the browser extension, so this is load-bearing.
        let timeline = two_people();
        let json = serde_json::to_string(&timeline).unwrap();
        let back: SpeakerTimeline = serde_json::from_str(&json).unwrap();

        assert_eq!(back, timeline);
    }
}
