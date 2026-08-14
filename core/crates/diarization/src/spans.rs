//! Choosing which audio is worth embedding.
//!
//! # Why not just embed every segment
//!
//! A speaker embedding is only as good as the audio behind it. Three things ruin one:
//!
//! - **Too little audio.** A 300 ms segment does not contain enough of a voice to place it.
//! - **Two people at once.** An embedding of overlapped speech lands between both speakers and
//!   belongs to neither, which is worse than having no embedding at all — it pulls two real
//!   clusters together.
//! - **The edges of a turn.** The start and end of a run border silence and hand-offs, and
//!   carry breath, hesitation, and the tail of whoever spoke before.
//!
//! So segments are merged into runs, overlaps are cut out, short remnants are dropped, and long
//! runs are clipped to their middle. What comes back is the audio most likely to identify a
//! voice, not simply the most audio.
//!
//! # Credit
//!
//! The approach — merge, subtract overlaps, clip to the middle, rank by duration — is adapted
//! from the `voiceprint` crate of Hyprnote (<https://github.com/fastrepl/hyprnote>),
//! Copyright (c) 2023-present Fastrepl, Inc., MIT licensed.

use crate::embedding::MIN_EMBEDDING_MS;

/// A stretch of one recording worth embedding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start_ms: i64,
    pub end_ms: i64,
    /// Which capture channel this came from, when the recording had more than one.
    ///
    /// Spans never merge across channels: the microphone and the system tap are different
    /// people by construction, and a run spanning both would be an embedding of two voices.
    pub channel: u8,
}

impl Span {
    pub fn new(start_ms: i64, end_ms: i64) -> Self {
        Self {
            start_ms,
            end_ms,
            channel: 0,
        }
    }

    pub fn on_channel(mut self, channel: u8) -> Self {
        self.channel = channel;
        self
    }

    pub fn duration_ms(&self) -> i64 {
        (self.end_ms - self.start_ms).max(0)
    }

    fn overlaps(&self, other: &Span) -> bool {
        self.channel == other.channel
            && self.start_ms < other.end_ms
            && other.start_ms < self.end_ms
    }
}

/// Tuning for [`select_spans`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanConfig {
    /// Gaps up to this long do not break a run.
    ///
    /// Above a within-sentence breath, below a hand-off: the same reasoning as a turn
    /// threshold, but used only to decide what audio to embed together, never to decide who
    /// was speaking.
    pub merge_gap_ms: i64,

    /// A span shorter than this is not embedded.
    pub min_span_ms: i64,

    /// A span longer than this is clipped to its middle.
    ///
    /// More audio stops helping well before a meeting's worth of it, and the extra costs
    /// inference time on every segment.
    pub max_span_ms: i64,
}

impl Default for SpanConfig {
    fn default() -> Self {
        Self {
            merge_gap_ms: 400,
            // The embedder's own floor. Below it, `embed` refuses anyway.
            min_span_ms: MIN_EMBEDDING_MS.max(1_000),
            max_span_ms: 10_000,
        }
    }
}

/// Merge segment timings into spans worth embedding.
///
/// Input need not be sorted. Output is in chronological order per channel.
pub fn select_spans(segments: &[Span], config: &SpanConfig) -> Vec<Span> {
    let mut sorted: Vec<Span> = segments
        .iter()
        .copied()
        .filter(|s| s.duration_ms() > 0)
        .collect();
    sorted.sort_by_key(|s| (s.channel, s.start_ms, s.end_ms));

    let mut runs: Vec<Span> = Vec::new();
    for span in sorted {
        match runs.last_mut() {
            Some(run)
                if run.channel == span.channel
                    && span.start_ms - run.end_ms <= config.merge_gap_ms =>
            {
                run.end_ms = run.end_ms.max(span.end_ms);
            }
            _ => runs.push(span),
        }
    }

    runs.into_iter()
        .map(|run| clip_to_middle(run, config.max_span_ms))
        .filter(|run| run.duration_ms() >= config.min_span_ms)
        .collect()
}

/// Remove any part of `span` that another speaker was also talking over.
///
/// Used when attribution is already known from somewhere else — separate channels, say — and
/// what is wanted is clean audio per speaker.
pub fn subtract_overlaps(span: Span, others: &[Span]) -> Vec<Span> {
    let mut pieces = vec![span];

    for other in others {
        let mut next = Vec::new();
        for piece in pieces {
            if !piece.overlaps(other) {
                next.push(piece);
                continue;
            }
            if other.start_ms > piece.start_ms {
                next.push(Span {
                    end_ms: other.start_ms,
                    ..piece
                });
            }
            if other.end_ms < piece.end_ms {
                next.push(Span {
                    start_ms: other.end_ms,
                    ..piece
                });
            }
        }
        pieces = next;
    }

    pieces
}

/// Keep the middle of a long span.
fn clip_to_middle(span: Span, max_span_ms: i64) -> Span {
    if span.duration_ms() <= max_span_ms {
        return span;
    }
    let excess = span.duration_ms() - max_span_ms;
    Span {
        start_ms: span.start_ms + excess / 2,
        end_ms: span.start_ms + excess / 2 + max_span_ms,
        ..span
    }
}

/// Cut a span's samples out of a recording.
///
/// Returns `None` when the span falls outside the audio, which happens if timings and samples
/// come from different recordings — better caught here than embedded as whatever noise
/// happened to be at those indices.
pub fn samples_for(span: &Span, samples: &[f32], sample_rate: u32) -> Option<Vec<f32>> {
    if sample_rate == 0 {
        return None;
    }

    let index = |ms: i64| -> usize { (ms.max(0) as u64 * sample_rate as u64 / 1000) as usize };
    let start = index(span.start_ms);
    let end = index(span.end_ms).min(samples.len());

    (start < end).then(|| samples[start..end].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start_ms: i64, end_ms: i64) -> Span {
        Span::new(start_ms, end_ms)
    }

    #[test]
    fn segments_separated_by_a_breath_become_one_span() {
        let spans = select_spans(
            &[span(0, 900), span(1_100, 2_000), span(2_200, 3_100)],
            &SpanConfig::default(),
        );

        assert_eq!(spans.len(), 1);
        assert_eq!((spans[0].start_ms, spans[0].end_ms), (0, 3_100));
    }

    #[test]
    fn a_long_gap_starts_a_new_span() {
        let spans = select_spans(
            &[span(0, 2_000), span(9_000, 11_000)],
            &SpanConfig::default(),
        );
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn spans_too_short_to_identify_a_voice_are_dropped() {
        let spans = select_spans(&[span(0, 300)], &SpanConfig::default());
        assert!(spans.is_empty(), "300 ms is not a voice");
    }

    /// Edges border silence and hand-offs; the middle is the part that sounds like the person.
    #[test]
    fn a_long_span_is_clipped_to_its_middle() {
        let config = SpanConfig {
            max_span_ms: 10_000,
            ..Default::default()
        };
        let spans = select_spans(&[span(0, 30_000)], &config);

        assert_eq!(spans.len(), 1);
        assert_eq!((spans[0].start_ms, spans[0].end_ms), (10_000, 20_000));
    }

    /// The microphone and the system tap are different people by construction.
    #[test]
    fn spans_never_merge_across_channels() {
        let spans = select_spans(
            &[
                span(0, 2_000).on_channel(0),
                span(2_100, 4_000).on_channel(1),
            ],
            &SpanConfig::default(),
        );

        assert_eq!(spans.len(), 2, "merged two channels into one voice");
        assert_eq!(spans[0].channel, 0);
        assert_eq!(spans[1].channel, 1);
    }

    #[test]
    fn unsorted_input_is_handled() {
        let spans = select_spans(
            &[span(2_200, 3_100), span(0, 900), span(1_100, 2_000)],
            &SpanConfig::default(),
        );
        assert_eq!(spans.len(), 1);
        assert_eq!((spans[0].start_ms, spans[0].end_ms), (0, 3_100));
    }

    #[test]
    fn zero_length_segments_are_ignored() {
        assert!(select_spans(&[span(100, 100)], &SpanConfig::default()).is_empty());
    }

    /// An embedding of two voices belongs to neither and pulls real clusters together.
    #[test]
    fn overlapped_speech_is_cut_out() {
        let pieces = subtract_overlaps(span(0, 6_000), &[span(3_500, 4_000)]);

        assert_eq!(
            pieces
                .iter()
                .map(|p| (p.start_ms, p.end_ms))
                .collect::<Vec<_>>(),
            vec![(0, 3_500), (4_000, 6_000)]
        );
    }

    #[test]
    fn overlap_on_a_different_channel_is_not_an_overlap() {
        let pieces = subtract_overlaps(
            span(0, 6_000).on_channel(0),
            &[span(3_500, 4_000).on_channel(1)],
        );
        assert_eq!(pieces.len(), 1, "channels do not overlap each other");
    }

    #[test]
    fn a_fully_covered_span_disappears() {
        assert!(subtract_overlaps(span(1_000, 2_000), &[span(0, 3_000)]).is_empty());
    }

    #[test]
    fn samples_are_cut_at_the_right_offsets() {
        let audio: Vec<f32> = (0..16_000).map(|i| i as f32).collect();
        let cut = samples_for(&span(500, 1_000), &audio, 16_000).expect("in range");

        assert_eq!(cut.len(), 8_000);
        assert_eq!(cut[0], 8_000.0);
    }

    #[test]
    fn a_span_outside_the_audio_is_refused_rather_than_silently_wrong() {
        let audio = vec![0.0; 1_600]; // 100 ms
        assert!(samples_for(&span(5_000, 6_000), &audio, 16_000).is_none());
    }
}
