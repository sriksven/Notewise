//! Putting platform names onto acoustically-found speakers.
//!
//! # Why combine them at all
//!
//! The two sources of speaker evidence in this crate fail in opposite directions.
//!
//! [`crate::EmbeddingDiarizer`] hears the audio, so its turn boundaries are as precise as the
//! segments themselves — but it can only ever say *these three stretches are the same voice*.
//! The labels are `Speaker 1..N`, and no amount of better clustering turns one into a name.
//!
//! [`crate::TimelineDiarizer`] has the names, because the platform routing the call knows who is
//! unmuted. But an active-speaker feed is coarse: it is reported on a poll, it lags the start of
//! a turn, and when two people talk at once it names one of them. Its boundaries are worse than
//! the transcript's own.
//!
//! So: cluster with the audio, name with the timeline. Each supplies exactly what the other
//! lacks.
//!
//! ```text
//!   audio ──→ EmbeddingDiarizer ──→ "Speaker 2" spans   ─┐
//!                                                        ├─→ majority vote ─→ "Marcus"
//!   platform events ──→ SpeakerTimeline ─────────────────┘
//! ```
//!
//! # Two structural wins, and one guarded loss
//!
//! **Over-splitting repairs itself.** A voice that clustering split in two — a speaker who moved
//! closer to their mic halfway through — produces two clusters that both vote for Marcus. Both
//! get named "Marcus", which merges them. The timeline fixes a clustering error for free.
//!
//! **Enrollment falls out of it.** A cluster that has been given a name is a labelled voiceprint.
//! Persisting that would let the same person be recognised in a later meeting where no platform
//! exists to ask — an in-person one. Deliberately not done here: see the design spec, a stored
//! voiceprint is biometric data about someone who is not the user and never consented.
//!
//! **Under-splitting is the dangerous case, and is refused rather than guessed.** If clustering
//! merged two people into one cluster, a plain majority vote would name the whole cluster after
//! whoever talked more, attributing one colleague's words to another. [`NamingConfig::min_share`]
//! is the guard: a cluster whose winner does not clearly dominate keeps its anonymous label. A
//! transcript reading `Speaker 2` is a mild disappointment; one that puts words in a named
//! colleague's mouth is a serious defect, and nothing downstream can tell it from a correct one.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::timeline::{ParticipantId, SpeakerTimeline};
use crate::voice::AudioDiarizer;
use crate::Result;

use notewise_transcription::{Segment, Transcript};

/// Tuning for [`NamedClusterDiarizer`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NamingConfig {
    /// The share of a cluster's attributed time its winner must hold to earn the name, in
    /// `0.0..=1.0`.
    ///
    /// This is the guard against a mis-clustered pair of speakers being named after whichever of
    /// them talked more. At 0.6 a cluster split 70/30 between two participants stays anonymous;
    /// one that is 95% Marcus becomes Marcus.
    ///
    /// Raising it trades names for caution — more clusters stay `Speaker N`. Lowering it trades
    /// caution for names, and the errors it admits are the expensive kind: a real person quoted
    /// saying something they did not say. 0.6 is deliberately nearer the cautious end.
    pub min_share: f32,

    /// A cluster with less than this much overlapping platform-reported speech keeps its
    /// anonymous label.
    ///
    /// A share is a ratio, and a ratio computed over 200 ms of evidence is noise that can easily
    /// read as 100%. This is the floor that stops a single stray overlap from naming a whole
    /// cluster.
    pub min_evidence_ms: i64,
}

impl Default for NamingConfig {
    fn default() -> Self {
        Self {
            min_share: 0.6,
            min_evidence_ms: 1_500,
        }
    }
}

/// Runs an acoustic diarizer, then renames its clusters from a platform timeline.
///
/// Wraps any [`AudioDiarizer`] rather than owning the clustering itself, so the acoustic half
/// stays independently testable and swappable.
#[derive(Debug)]
pub struct NamedClusterDiarizer<D> {
    acoustic: D,
    timeline: SpeakerTimeline,
    config: NamingConfig,
}

impl<D> NamedClusterDiarizer<D> {
    pub fn new(acoustic: D, timeline: SpeakerTimeline) -> Self {
        Self {
            acoustic,
            timeline,
            config: NamingConfig::default(),
        }
    }

    pub fn with_config(mut self, config: NamingConfig) -> Self {
        self.config = config;
        self
    }

    pub fn timeline(&self) -> &SpeakerTimeline {
        &self.timeline
    }

    /// Decide a name for each anonymous cluster label.
    ///
    /// Returns only the clusters that earned one; callers keep the original label for the rest.
    fn resolve_names(&self, clustered: &Transcript) -> HashMap<String, String> {
        // For each anonymous label, how many milliseconds each participant was reported over.
        let mut votes: HashMap<&str, HashMap<ParticipantId, i64>> = HashMap::new();

        for segment in &clustered.segments {
            let Some(label) = segment.speaker.as_deref() else {
                continue;
            };
            let tally = votes.entry(label).or_default();

            // Every turn touching this segment votes, weighted by how much of the segment it
            // covers. Using every overlapping turn rather than only the winner means a cluster
            // that spans two people shows up as split rather than unanimous — which is exactly
            // what `min_share` needs to see in order to refuse it.
            for turn in self.timeline.turns() {
                if turn.start_ms >= segment.end_ms {
                    break;
                }
                let overlap =
                    (turn.end_ms.min(segment.end_ms) - turn.start_ms.max(segment.start_ms)).max(0);
                if overlap > 0 {
                    *tally.entry(turn.participant.clone()).or_insert(0) += overlap;
                }
            }
        }

        let mut names = HashMap::new();

        for (label, tally) in votes {
            let total: i64 = tally.values().sum();
            if total < self.config.min_evidence_ms {
                continue;
            }

            let Some((winner, best)) = tally.iter().max_by_key(|(_, ms)| **ms) else {
                continue;
            };
            if (*best as f32 / total as f32) < self.config.min_share {
                continue;
            }

            if let Some(name) = self.timeline.name_of(winner) {
                names.insert(label.to_string(), name.to_string());
            }
        }

        names
    }
}

impl<D: AudioDiarizer> AudioDiarizer for NamedClusterDiarizer<D> {
    fn name(&self) -> &str {
        "named-cluster"
    }

    fn diarize(
        &self,
        transcript: &Transcript,
        samples: &[f32],
        sample_rate: u32,
    ) -> Result<Transcript> {
        let clustered = self.acoustic.diarize(transcript, samples, sample_rate)?;

        // No platform evidence: the acoustic labels stand on their own. Anonymous and correct
        // beats named and invented.
        if self.timeline.is_empty() {
            return Ok(clustered);
        }

        let names = self.resolve_names(&clustered);
        if names.is_empty() {
            return Ok(clustered);
        }

        let segments: Vec<Segment> = clustered
            .segments
            .iter()
            .map(|segment| {
                let mut segment = segment.clone();
                if let Some(label) = segment.speaker.as_deref() {
                    if let Some(name) = names.get(label) {
                        segment.speaker = Some(name.clone());
                    }
                }
                segment
            })
            .collect();

        Ok(Transcript::new(segments))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::Participant;

    /// Stands in for the acoustic half, so these tests exercise the naming logic without a model.
    ///
    /// Labels segments by a caller-supplied cluster assignment — the same shape
    /// [`crate::EmbeddingDiarizer`] produces, and the only part of it this code depends on.
    #[derive(Debug)]
    struct FakeClusterer {
        labels: Vec<&'static str>,
    }

    impl AudioDiarizer for FakeClusterer {
        fn name(&self) -> &str {
            "fake-clusterer"
        }

        fn diarize(
            &self,
            transcript: &Transcript,
            _samples: &[f32],
            _sample_rate: u32,
        ) -> Result<Transcript> {
            Ok(Transcript::new(
                transcript
                    .segments
                    .iter()
                    .zip(&self.labels)
                    .map(|(segment, label)| {
                        let mut segment = segment.clone();
                        segment.speaker = Some((*label).to_string());
                        segment
                    })
                    .collect(),
            ))
        }
    }

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

    fn timeline_of(turns: &[(&str, &str, i64, i64)]) -> SpeakerTimeline {
        let mut timeline = SpeakerTimeline::new();
        for (id, name, _, _) in turns {
            timeline.upsert_participant(Participant::new(*id, *name));
        }
        for (id, _, start, end) in turns {
            timeline
                .add_turn(ParticipantId::new(*id), *start, *end)
                .unwrap();
        }
        timeline
    }

    /// The headline behaviour: anonymous clusters come out with real names.
    #[test]
    fn clusters_are_renamed_to_platform_participants() {
        let acoustic = FakeClusterer {
            labels: vec!["Speaker 1", "Speaker 2"],
        };
        let timeline = timeline_of(&[("p1", "Priya", 0, 10_000), ("p2", "Marcus", 10_000, 20_000)]);

        let output = NamedClusterDiarizer::new(acoustic, timeline)
            .diarize(&transcript(&[(0, 10_000), (10_000, 20_000)]), &[], 16_000)
            .unwrap();

        assert_eq!(
            speakers(&output),
            vec![Some("Priya".to_string()), Some("Marcus".to_string())]
        );
    }

    /// A voice clustering split in two is put back together by the names.
    #[test]
    fn an_over_split_voice_is_merged_by_naming() {
        // Clustering thought these were two people; the platform says both were Priya.
        let acoustic = FakeClusterer {
            labels: vec!["Speaker 1", "Speaker 3"],
        };
        let timeline = timeline_of(&[("p1", "Priya", 0, 20_000)]);

        let output = NamedClusterDiarizer::new(acoustic, timeline)
            .diarize(&transcript(&[(0, 10_000), (10_000, 20_000)]), &[], 16_000)
            .unwrap();

        assert_eq!(
            speakers(&output),
            vec![Some("Priya".to_string()), Some("Priya".to_string())],
            "two clusters voting for one person become one person"
        );
    }

    /// The failure this type exists to prevent.
    #[test]
    fn an_under_split_cluster_is_left_anonymous_rather_than_misnamed() {
        // One cluster covering both people, roughly evenly. Naming it would put one person's
        // words in the other's mouth.
        let acoustic = FakeClusterer {
            labels: vec!["Speaker 1", "Speaker 1"],
        };
        let timeline = timeline_of(&[("p1", "Priya", 0, 10_000), ("p2", "Marcus", 10_000, 20_000)]);

        let output = NamedClusterDiarizer::new(acoustic, timeline)
            .diarize(&transcript(&[(0, 10_000), (10_000, 20_000)]), &[], 16_000)
            .unwrap();

        assert_eq!(
            speakers(&output),
            vec![Some("Speaker 1".to_string()), Some("Speaker 1".to_string())],
            "a 50/50 cluster must not be named after either person"
        );
    }

    /// A clear majority does earn the name — the guard must not refuse everything.
    #[test]
    fn a_dominant_participant_earns_the_name_despite_an_interjection() {
        let acoustic = FakeClusterer {
            labels: vec!["Speaker 1", "Speaker 1"],
        };
        // Priya holds 19s of 20s; Marcus interjects for one.
        let timeline = timeline_of(&[("p1", "Priya", 0, 19_000), ("p2", "Marcus", 19_000, 20_000)]);

        let output = NamedClusterDiarizer::new(acoustic, timeline)
            .diarize(&transcript(&[(0, 10_000), (10_000, 20_000)]), &[], 16_000)
            .unwrap();

        assert_eq!(
            speakers(&output),
            vec![Some("Priya".to_string()), Some("Priya".to_string())]
        );
    }

    #[test]
    fn a_cluster_with_too_little_evidence_stays_anonymous() {
        let acoustic = FakeClusterer {
            labels: vec!["Speaker 1"],
        };
        // 200 ms of overlap is a unanimous vote on almost no evidence.
        let timeline = timeline_of(&[("p1", "Priya", 9_800, 10_000)]);

        let output = NamedClusterDiarizer::new(acoustic, timeline)
            .diarize(&transcript(&[(0, 10_000)]), &[], 16_000)
            .unwrap();

        assert_eq!(
            speakers(&output),
            vec![Some("Speaker 1".to_string())],
            "100% of 200ms is not evidence"
        );
    }

    #[test]
    fn an_empty_timeline_leaves_the_acoustic_labels_untouched() {
        let acoustic = FakeClusterer {
            labels: vec!["Speaker 1", "Speaker 2"],
        };

        let output = NamedClusterDiarizer::new(acoustic, SpeakerTimeline::new())
            .diarize(&transcript(&[(0, 10_000), (10_000, 20_000)]), &[], 16_000)
            .unwrap();

        assert_eq!(
            speakers(&output),
            vec![Some("Speaker 1".to_string()), Some("Speaker 2".to_string())]
        );
    }

    /// Segments the platform never reported on keep the acoustic label — a hole gets an
    /// anonymous name, never a borrowed one.
    #[test]
    fn segments_outside_the_timeline_keep_their_cluster_label() {
        let acoustic = FakeClusterer {
            labels: vec!["Speaker 1", "Speaker 2"],
        };
        // Only the first segment has platform coverage.
        let timeline = timeline_of(&[("p1", "Priya", 0, 10_000)]);

        let output = NamedClusterDiarizer::new(acoustic, timeline)
            .diarize(&transcript(&[(0, 10_000), (50_000, 60_000)]), &[], 16_000)
            .unwrap();

        assert_eq!(
            speakers(&output),
            vec![Some("Priya".to_string()), Some("Speaker 2".to_string())],
            "the uncovered cluster stays anonymous rather than inheriting a name"
        );
    }

    #[test]
    fn a_raised_min_share_refuses_a_borderline_cluster() {
        let acoustic = FakeClusterer {
            labels: vec!["Speaker 1", "Speaker 1"],
        };
        let timeline = timeline_of(&[("p1", "Priya", 0, 13_000), ("p2", "Marcus", 13_000, 20_000)]);

        // 65% Priya: named at the default 0.6, refused at 0.9.
        let permissive = NamedClusterDiarizer::new(
            FakeClusterer {
                labels: vec!["Speaker 1", "Speaker 1"],
            },
            timeline.clone(),
        );
        let strict = NamedClusterDiarizer::new(acoustic, timeline).with_config(NamingConfig {
            min_share: 0.9,
            ..NamingConfig::default()
        });

        let input = transcript(&[(0, 10_000), (10_000, 20_000)]);

        assert_eq!(
            speakers(&permissive.diarize(&input, &[], 16_000).unwrap())[0],
            Some("Priya".to_string())
        );
        assert_eq!(
            speakers(&strict.diarize(&input, &[], 16_000).unwrap())[0],
            Some("Speaker 1".to_string())
        );
    }

    #[test]
    fn naming_preserves_text_and_timings() {
        let acoustic = FakeClusterer {
            labels: vec!["Speaker 1", "Speaker 2"],
        };
        let timeline = timeline_of(&[("p1", "Priya", 0, 10_000), ("p2", "Marcus", 10_000, 20_000)]);
        let input = transcript(&[(0, 10_000), (10_000, 20_000)]);

        let output = NamedClusterDiarizer::new(acoustic, timeline)
            .diarize(&input, &[], 16_000)
            .unwrap();

        for (before, after) in input.segments.iter().zip(&output.segments) {
            assert_eq!(before.text, after.text);
            assert_eq!(
                (before.start_ms, before.end_ms),
                (after.start_ms, after.end_ms)
            );
        }
    }

    /// Five people, each their own cluster, all named. The requirement that started this.
    #[test]
    fn five_clusters_get_five_names() {
        let labels = vec![
            "Speaker 1",
            "Speaker 2",
            "Speaker 3",
            "Speaker 4",
            "Speaker 5",
        ];
        let names = ["Priya", "Marcus", "Ana", "Jun", "Tobi"];

        let turns: Vec<(&str, &str, i64, i64)> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let start = i as i64 * 10_000;
                (
                    ["p0", "p1", "p2", "p3", "p4"][i],
                    *name,
                    start,
                    start + 10_000,
                )
            })
            .collect();

        let spans: Vec<(i64, i64)> = (0..5).map(|i| (i * 10_000, i * 10_000 + 10_000)).collect();

        let output = NamedClusterDiarizer::new(FakeClusterer { labels }, timeline_of(&turns))
            .diarize(&transcript(&spans), &[], 16_000)
            .unwrap();

        assert_eq!(
            speakers(&output),
            names
                .iter()
                .map(|n| Some(n.to_string()))
                .collect::<Vec<_>>()
        );
    }
}
