//! Agglomerative clustering of speaker embeddings.
//!
//! # Why agglomerative, and why cosine
//!
//! The number of speakers in a meeting is not known in advance, which rules out k-means and
//! anything else that takes `k` as input. Asking the user how many people are in their own
//! meeting is not an answer either — they will get it wrong when someone joins late, and the
//! whole transcript is then mislabelled.
//!
//! Agglomerative clustering with a **distance threshold** instead of a cluster count is the
//! standard choice for exactly this reason: it stops merging when the nearest two clusters stop
//! looking like the same person, whatever number that leaves.
//!
//! Distance is cosine, on L2-normalised embeddings, because speaker embedding networks are
//! trained with angular margin losses (AAM-softmax and relatives). The geometry they learn is
//! angular — magnitude carries loudness and channel, not identity — so Euclidean distance on
//! raw embeddings measures partly the wrong thing.
//!
//! # Average linkage
//!
//! Single linkage chains: two speakers connected by one ambiguous segment collapse into one
//! cluster. Complete linkage is the opposite, splitting a speaker whose voice varies across a
//! long meeting. Average linkage sits between them and is what the diarization literature
//! settled on.

use serde::{Deserialize, Serialize};

/// Tuning for [`cluster`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// Stop merging once the closest pair is at least this far apart, in cosine distance
    /// (`0.0` identical, `2.0` opposite).
    ///
    /// The single most consequential number here. Too low and one person becomes several; too
    /// high and a meeting becomes one speaker. 0.55 is around where CAM++ and WeSpeaker
    /// embeddings separate speakers on 16 kHz conversational audio.
    pub threshold: f32,

    /// Never produce more than this many speakers.
    ///
    /// A backstop against a noisy recording fragmenting into dozens of "speakers", which is
    /// worse than useless — it makes the transcript unreadable. Meetings with more than this
    /// many active speakers exist, but a wrong 12 is more likely than a real 12.
    pub max_speakers: usize,

    /// Segments shorter than this are labelled by their neighbours rather than clustered.
    ///
    /// An embedding from under a second of audio is dominated by phonetic content rather than
    /// speaker identity, and clustering on it adds noise instead of information.
    pub min_duration_ms: i64,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            threshold: 0.55,
            max_speakers: 10,
            min_duration_ms: 1_000,
        }
    }
}

/// L2-normalise in place, so cosine distance is a dot product.
///
/// A zero vector is left alone: normalising it would divide by zero, and it carries no
/// direction to preserve anyway.
pub fn normalize(embedding: &mut [f32]) {
    let norm = embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for value in embedding.iter_mut() {
            *value /= norm;
        }
    }
}

/// Cosine distance between two embeddings, in `0.0..=2.0`.
///
/// Computes the norms rather than assuming normalised input. Silently returning a wrong
/// distance for un-normalised vectors would be a very hard bug to see: clustering would still
/// produce clusters, just worse ones.
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "embedding dimensions differ");

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let denominator = (norm_a.sqrt() * norm_b.sqrt()).max(f32::EPSILON);
    (1.0 - dot / denominator).clamp(0.0, 2.0)
}

/// Group embeddings into speakers.
///
/// Returns one cluster index per input, in input order. Indices are assigned by first
/// appearance, so the first person to speak is always speaker 0 — stable labels matter,
/// because a transcript whose speaker numbering changes between runs cannot be diffed or
/// referred to.
pub fn cluster(embeddings: &[Vec<f32>], config: ClusterConfig) -> Vec<usize> {
    match embeddings.len() {
        0 => return Vec::new(),
        1 => return vec![0],
        _ => {}
    }

    // Each embedding starts in its own cluster.
    let mut members: Vec<Vec<usize>> = (0..embeddings.len()).map(|i| vec![i]).collect();

    loop {
        if members.len() <= 1 {
            break;
        }

        let mut best: Option<(usize, usize, f32)> = None;

        for i in 0..members.len() {
            for j in (i + 1)..members.len() {
                let distance = average_linkage(embeddings, &members[i], &members[j]);
                if best.is_none_or(|(_, _, d)| distance < d) {
                    best = Some((i, j, distance));
                }
            }
        }

        let Some((i, j, distance)) = best else { break };

        // Stop when the closest pair no longer looks like one person — unless there are still
        // more clusters than allowed, in which case merging the closest pair is the least-bad
        // way down to the cap.
        if distance >= config.threshold && members.len() <= config.max_speakers {
            break;
        }

        let merged = members.remove(j);
        members[i].extend(merged);
    }

    // Relabel by first appearance so numbering is stable and meaningful.
    let mut labels = vec![0usize; embeddings.len()];
    let mut order: Vec<(usize, usize)> = members
        .iter()
        .enumerate()
        .map(|(cluster, m)| (*m.iter().min().expect("non-empty cluster"), cluster))
        .collect();
    order.sort_unstable();

    for (speaker, (_, cluster)) in order.into_iter().enumerate() {
        for &index in &members[cluster] {
            labels[index] = speaker;
        }
    }

    labels
}

/// Mean pairwise distance between two clusters.
fn average_linkage(embeddings: &[Vec<f32>], a: &[usize], b: &[usize]) -> f32 {
    let mut total = 0.0f32;
    for &i in a {
        for &j in b {
            total += cosine_distance(&embeddings[i], &embeddings[j]);
        }
    }
    total / (a.len() * b.len()) as f32
}

/// How confident the clustering is, in `0.0..=1.0`.
///
/// The ratio of the tightest within-cluster distance to the loosest between-cluster distance:
/// well-separated speakers score high, a room where everyone sounds alike scores low. Surfaced
/// so the UI can say "speakers are a guess" rather than presenting labels as fact.
pub fn confidence(embeddings: &[Vec<f32>], labels: &[usize]) -> f32 {
    if embeddings.len() < 2 || labels.len() != embeddings.len() {
        return 0.0;
    }

    let speakers = labels.iter().max().copied().unwrap_or(0) + 1;
    if speakers < 2 {
        // One speaker: there is nothing to have got wrong, but also nothing that was verified.
        return 0.5;
    }

    let mut within = Vec::new();
    let mut between = Vec::new();

    for i in 0..embeddings.len() {
        for j in (i + 1)..embeddings.len() {
            let distance = cosine_distance(&embeddings[i], &embeddings[j]);
            if labels[i] == labels[j] {
                within.push(distance);
            } else {
                between.push(distance);
            }
        }
    }

    if within.is_empty() || between.is_empty() {
        return 0.5;
    }

    let mean_within = within.iter().sum::<f32>() / within.len() as f32;
    let mean_between = between.iter().sum::<f32>() / between.len() as f32;

    if mean_between <= f32::EPSILON {
        return 0.0;
    }

    // 1 - within/between: identical clusters score 0, perfectly separated approaches 1.
    (1.0 - mean_within / mean_between).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic embedding near `centre`, so tests describe speakers rather than numbers.
    fn near(centre: &[f32], jitter: f32, seed: u64) -> Vec<f32> {
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut embedding: Vec<f32> = centre
            .iter()
            .map(|v| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let unit = ((state >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0;
                v + unit * jitter
            })
            .collect();
        normalize(&mut embedding);
        embedding
    }

    fn alice() -> Vec<f32> {
        vec![1.0, 0.2, 0.0, 0.1, 0.0, 0.0, 0.0, 0.0]
    }

    fn bob() -> Vec<f32> {
        vec![0.0, 0.0, 0.1, 0.0, 1.0, 0.2, 0.0, 0.0]
    }

    fn carol() -> Vec<f32> {
        vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.3]
    }

    // ------------------------------------------------------------------ distance

    #[test]
    fn identical_embeddings_are_zero_distance() {
        let a = alice();
        assert!(cosine_distance(&a, &a) < 1e-6);
    }

    #[test]
    fn orthogonal_embeddings_are_distance_one() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!((cosine_distance(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn opposite_embeddings_are_distance_two() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_distance(&a, &b) - 2.0).abs() < 1e-6);
    }

    /// Cosine must ignore magnitude — that is the whole reason for using it. Loudness and
    /// channel gain live in the magnitude; identity does not.
    #[test]
    fn distance_ignores_magnitude() {
        let quiet = vec![0.1, 0.2, 0.3];
        let loud: Vec<f32> = quiet.iter().map(|v| v * 50.0).collect();
        assert!(cosine_distance(&quiet, &loud) < 1e-5);
    }

    #[test]
    fn a_zero_embedding_does_not_produce_nan() {
        let zero = vec![0.0; 8];
        let distance = cosine_distance(&zero, &alice());
        assert!(distance.is_finite(), "got {distance}");
        assert!((0.0..=2.0).contains(&distance));
    }

    #[test]
    fn normalizing_a_zero_vector_leaves_it_alone() {
        let mut zero = vec![0.0f32; 4];
        normalize(&mut zero);
        assert!(zero.iter().all(|v| v.is_finite() && *v == 0.0));
    }

    #[test]
    fn normalize_produces_unit_length() {
        let mut embedding = vec![3.0, 4.0];
        normalize(&mut embedding);
        let norm = embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    // ------------------------------------------------------------------ clustering

    #[test]
    fn no_embeddings_produce_no_labels() {
        assert!(cluster(&[], ClusterConfig::default()).is_empty());
    }

    #[test]
    fn a_single_embedding_is_one_speaker() {
        assert_eq!(cluster(&[alice()], ClusterConfig::default()), vec![0]);
    }

    /// The core claim: two people, clustered apart, without being told there are two.
    #[test]
    fn two_speakers_are_separated_without_being_told_how_many() {
        let embeddings = vec![
            near(&alice(), 0.05, 1),
            near(&bob(), 0.05, 2),
            near(&alice(), 0.05, 3),
            near(&bob(), 0.05, 4),
        ];

        let labels = cluster(&embeddings, ClusterConfig::default());
        assert_eq!(labels.len(), 4);
        assert_eq!(labels[0], labels[2], "both Alice segments: {labels:?}");
        assert_eq!(labels[1], labels[3], "both Bob segments: {labels:?}");
        assert_ne!(labels[0], labels[1], "Alice and Bob merged: {labels:?}");
    }

    #[test]
    fn three_speakers_are_separated() {
        let embeddings = vec![
            near(&alice(), 0.04, 10),
            near(&bob(), 0.04, 11),
            near(&carol(), 0.04, 12),
            near(&alice(), 0.04, 13),
            near(&carol(), 0.04, 14),
        ];

        let labels = cluster(&embeddings, ClusterConfig::default());
        let speakers: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(speakers.len(), 3, "{labels:?}");
        assert_eq!(labels[0], labels[3]);
        assert_eq!(labels[2], labels[4]);
    }

    /// One person must not be split by ordinary within-speaker variation. A transcript where
    /// one person appears as four speakers is worse than one with no labels at all.
    #[test]
    fn one_speaker_stays_one_speaker() {
        let embeddings: Vec<Vec<f32>> = (0..8).map(|i| near(&alice(), 0.08, i)).collect();
        let labels = cluster(&embeddings, ClusterConfig::default());

        assert!(
            labels.iter().all(|&l| l == 0),
            "one voice fragmented into {} speakers: {labels:?}",
            labels.iter().max().unwrap() + 1
        );
    }

    /// Labels are assigned by first appearance, so the first person to speak is speaker 0.
    /// Unstable numbering makes transcripts undiffable between runs.
    #[test]
    fn speaker_numbering_follows_first_appearance() {
        let embeddings = vec![
            near(&bob(), 0.03, 21),
            near(&alice(), 0.03, 22),
            near(&bob(), 0.03, 23),
        ];

        let labels = cluster(&embeddings, ClusterConfig::default());
        assert_eq!(labels[0], 0, "the first segment must be speaker 0");
        assert_eq!(labels[2], 0);
        assert_eq!(labels[1], 1);
    }

    /// The cap is a backstop against a noisy recording fragmenting into dozens of speakers.
    #[test]
    fn the_speaker_cap_is_respected() {
        // Eight mutually distant voices, capped at three.
        let embeddings: Vec<Vec<f32>> = (0..8)
            .map(|i| {
                let mut e = vec![0.0f32; 8];
                e[i] = 1.0;
                e
            })
            .collect();

        let labels = cluster(
            &embeddings,
            ClusterConfig {
                max_speakers: 3,
                ..Default::default()
            },
        );

        let speakers = labels.iter().max().unwrap() + 1;
        assert!(speakers <= 3, "produced {speakers} speakers: {labels:?}");
    }

    /// A threshold of zero must not merge anything; a threshold of two must merge everything.
    /// The extremes prove the threshold is actually driving the decision.
    #[test]
    fn the_threshold_controls_merging() {
        let embeddings = vec![
            near(&alice(), 0.02, 31),
            near(&bob(), 0.02, 32),
            near(&carol(), 0.02, 33),
        ];

        let split = cluster(
            &embeddings,
            ClusterConfig {
                threshold: 0.0,
                max_speakers: 10,
                ..Default::default()
            },
        );
        assert_eq!(split, vec![0, 1, 2], "nothing should merge");

        let merged = cluster(
            &embeddings,
            ClusterConfig {
                threshold: 2.0,
                max_speakers: 10,
                ..Default::default()
            },
        );
        assert!(merged.iter().all(|&l| l == 0), "everything should merge");
    }

    /// Average linkage, not single linkage. Under single linkage one ambiguous segment sitting
    /// between two speakers chains them into a single cluster.
    #[test]
    fn an_ambiguous_segment_does_not_chain_two_speakers_together() {
        let mut bridge: Vec<f32> = alice()
            .iter()
            .zip(bob())
            .map(|(a, b)| (a + b) / 2.0)
            .collect();
        normalize(&mut bridge);

        let embeddings = vec![
            near(&alice(), 0.02, 41),
            near(&alice(), 0.02, 42),
            bridge,
            near(&bob(), 0.02, 43),
            near(&bob(), 0.02, 44),
        ];

        let labels = cluster(&embeddings, ClusterConfig::default());
        assert_ne!(
            labels[0], labels[4],
            "Alice and Bob chained through the ambiguous segment: {labels:?}"
        );
    }

    #[test]
    fn clustering_is_deterministic() {
        let (a, b) = (alice(), bob());
        let embeddings: Vec<Vec<f32>> = (0..6)
            .map(|i| near(if i % 2 == 0 { &a } else { &b }, 0.05, i))
            .collect();

        let first = cluster(&embeddings, ClusterConfig::default());
        for _ in 0..5 {
            assert_eq!(cluster(&embeddings, ClusterConfig::default()), first);
        }
    }

    // ------------------------------------------------------------------ confidence

    #[test]
    fn well_separated_speakers_score_higher_than_similar_ones() {
        let distinct = vec![
            near(&alice(), 0.02, 51),
            near(&bob(), 0.02, 52),
            near(&alice(), 0.02, 53),
            near(&bob(), 0.02, 54),
        ];
        let distinct_labels = cluster(&distinct, ClusterConfig::default());

        // Two voices that barely differ, forced apart by a tight threshold.
        let similar: Vec<Vec<f32>> = (0..4).map(|i| near(&alice(), 0.03, 60 + i)).collect();
        let similar_labels = cluster(
            &similar,
            ClusterConfig {
                threshold: 0.001,
                ..Default::default()
            },
        );

        let high = confidence(&distinct, &distinct_labels);
        let low = confidence(&similar, &similar_labels);

        assert!(
            high > low,
            "distinct {high:.3} should beat similar {low:.3}"
        );
        assert!((0.0..=1.0).contains(&high));
        assert!((0.0..=1.0).contains(&low));
    }

    #[test]
    fn confidence_is_bounded_and_safe_on_degenerate_input() {
        assert_eq!(confidence(&[], &[]), 0.0);
        assert_eq!(confidence(&[alice()], &[0]), 0.0);
        // Mismatched lengths must not panic or index out of bounds.
        assert_eq!(confidence(&[alice(), bob()], &[0]), 0.0);
    }

    /// A single speaker is neither confidently right nor wrong: nothing was separated, so
    /// nothing was verified.
    #[test]
    fn a_single_speaker_reports_middling_confidence() {
        let embeddings: Vec<Vec<f32>> = (0..4).map(|i| near(&alice(), 0.05, 70 + i)).collect();
        let labels = vec![0; 4];
        assert_eq!(confidence(&embeddings, &labels), 0.5);
    }
}
