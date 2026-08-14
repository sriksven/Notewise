//! Separating speakers by voice.
//!
//! The half of diarization that needs the audio. [`crate::PauseDiarizer`] guesses from timings
//! and [`crate::SingleSpeakerDiarizer`] declines to guess at all; this one listens.
//!
//! # Shape
//!
//! ```text
//!   segments ─→ select_spans ─→ SpeakerEmbedder ─→ cluster ─→ labels back onto segments
//!               (clean audio)    (a vector per     (which
//!                                 span)             vectors are
//!                                                   one voice)
//! ```
//!
//! # Why this is a separate trait
//!
//! [`crate::Diarizer`] takes a transcript and nothing else, which is the right interface for
//! something reasoning about timings and the only interface an imported transcript can satisfy.
//! Voices cannot be recovered from timings, so pretending this fits that trait would mean an
//! implementation that fails at runtime whenever the audio is not to hand. A separate trait
//! makes the requirement visible in the type.

use crate::cluster::{cluster, ClusterConfig};
use crate::embedding::SpeakerEmbedder;
use crate::spans::{samples_for, select_spans, Span, SpanConfig};
use crate::Result;

use notewise_transcription::{Segment, Transcript};

/// Assigns speakers using the recorded audio.
pub trait AudioDiarizer: std::fmt::Debug {
    fn name(&self) -> &str;

    /// Label a transcript, given the audio it was transcribed from.
    ///
    /// `samples` must be mono at `sample_rate`, covering the same time base as the segments.
    fn diarize(
        &self,
        transcript: &Transcript,
        samples: &[f32],
        sample_rate: u32,
    ) -> Result<Transcript>;
}

/// Clusters speaker embeddings to separate voices.
#[derive(Debug)]
pub struct EmbeddingDiarizer {
    embedder: SpeakerEmbedder,
    spans: SpanConfig,
    clustering: ClusterConfig,
}

impl EmbeddingDiarizer {
    pub fn new(embedder: SpeakerEmbedder) -> Self {
        Self {
            embedder,
            spans: SpanConfig::default(),
            clustering: ClusterConfig::default(),
        }
    }

    pub fn with_span_config(mut self, spans: SpanConfig) -> Self {
        self.spans = spans;
        self
    }

    pub fn with_cluster_config(mut self, clustering: ClusterConfig) -> Self {
        self.clustering = clustering;
        self
    }

    fn label(index: usize) -> String {
        format!("Speaker {}", index + 1)
    }
}

impl AudioDiarizer for EmbeddingDiarizer {
    fn name(&self) -> &str {
        "speaker-embedding"
    }

    fn diarize(
        &self,
        transcript: &Transcript,
        samples: &[f32],
        sample_rate: u32,
    ) -> Result<Transcript> {
        if transcript.is_empty() {
            return Ok(transcript.clone());
        }

        let segment_spans: Vec<Span> = transcript
            .segments
            .iter()
            .map(|s| Span::new(s.start_ms, s.end_ms))
            .collect();

        let spans = select_spans(&segment_spans, &self.spans);

        // Nothing long enough to identify anyone from. One speaker is the honest answer, and
        // an error would be wrong — a short meeting is not a failure.
        if spans.is_empty() {
            return Ok(label_all(transcript, &Self::label(0)));
        }

        let mut embedded: Vec<(Span, Vec<f32>)> = Vec::with_capacity(spans.len());
        for span in spans {
            let Some(audio) = samples_for(&span, samples, sample_rate) else {
                continue;
            };
            match self.embedder.embed(&audio, sample_rate) {
                Ok(embedding) => embedded.push((span, embedding)),
                // One span failing is not the recording failing. Skip it: the remaining spans
                // still separate the voices, just with less evidence.
                Err(e) => tracing::debug!(
                    start_ms = span.start_ms,
                    error = %e,
                    "could not embed a span; skipping it"
                ),
            }
        }

        if embedded.is_empty() {
            return Ok(label_all(transcript, &Self::label(0)));
        }

        let vectors: Vec<Vec<f32>> = embedded.iter().map(|(_, v)| v.clone()).collect();
        let assignments = cluster(&vectors, self.clustering);

        let labelled: Vec<(Span, usize)> = embedded
            .iter()
            .map(|(span, _)| *span)
            .zip(assignments.iter().copied())
            .collect();

        let segments: Vec<Segment> = transcript
            .segments
            .iter()
            .map(|segment| {
                let mut segment = segment.clone();
                segment.speaker = Some(Self::label(nearest_cluster(segment.start_ms, &labelled)));
                segment
            })
            .collect();

        Ok(Transcript::new(segments))
    }
}

/// The cluster of the span covering this time, or the closest one.
///
/// Segments too short to embed still need a label, and the speaker who was talking either side
/// of them is a far better guess than leaving a hole in the transcript.
fn nearest_cluster(start_ms: i64, labelled: &[(Span, usize)]) -> usize {
    let mut best = (i64::MAX, 0usize);

    for (span, assignment) in labelled {
        if start_ms >= span.start_ms && start_ms < span.end_ms {
            return *assignment;
        }
        let distance = if start_ms < span.start_ms {
            span.start_ms - start_ms
        } else {
            start_ms - span.end_ms
        };
        if distance < best.0 {
            best = (distance, *assignment);
        }
    }

    best.1
}

fn label_all(transcript: &Transcript, speaker: &str) -> Transcript {
    Transcript::new(
        transcript
            .segments
            .iter()
            .map(|segment| {
                let mut segment = segment.clone();
                segment.speaker = Some(speaker.to_string());
                segment
            })
            .collect(),
    )
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

    #[test]
    fn the_span_covering_a_segment_wins() {
        let labelled = vec![(Span::new(0, 5_000), 0), (Span::new(10_000, 15_000), 1)];

        assert_eq!(nearest_cluster(1_000, &labelled), 0);
        assert_eq!(nearest_cluster(12_000, &labelled), 1);
    }

    /// A segment too short to embed still gets the speaker who was talking around it.
    #[test]
    fn a_segment_between_spans_takes_the_closer_one() {
        let labelled = vec![(Span::new(0, 5_000), 0), (Span::new(10_000, 15_000), 1)];

        assert_eq!(nearest_cluster(5_500, &labelled), 0, "closer to the first");
        assert_eq!(nearest_cluster(9_500, &labelled), 1, "closer to the second");
    }

    #[test]
    fn labelling_everything_leaves_text_and_timings_alone() {
        let input = transcript(&[(0, 1_000), (2_000, 3_000)]);
        let output = label_all(&input, "Speaker 1");

        assert!(output
            .segments
            .iter()
            .all(|s| s.speaker.as_deref() == Some("Speaker 1")));
        for (before, after) in input.segments.iter().zip(output.segments.iter()) {
            assert_eq!(before.text, after.text);
            assert_eq!(
                (before.start_ms, before.end_ms),
                (after.start_ms, after.end_ms)
            );
        }
    }

    /// Needs the ONNX feature and a downloaded speaker model, so it cannot run in a default
    /// build. Without it, a green suite must not suggest voice separation works.
    #[tokio::test]
    #[cfg(feature = "onnx")]
    #[ignore = "requires the onnx feature and a downloaded speaker embedding model"]
    async fn two_speakers_are_separated_by_voice() {
        let model = std::env::var("NOTEWISE_SPEAKER_MODEL").expect("NOTEWISE_SPEAKER_MODEL");
        let wav = std::env::var("NOTEWISE_TWO_SPEAKER_WAV").expect("NOTEWISE_TWO_SPEAKER_WAV");

        let embedder = SpeakerEmbedder::load(&model).expect("speaker model");
        let diarizer = EmbeddingDiarizer::new(embedder);

        use notewise_audio_capture::AudioSource;
        let mut source = notewise_audio_capture::FileSource::open_wav(&wav).expect("wav");
        let mut samples = Vec::new();
        while let Some(frame) = source.next_frame().expect("frame") {
            samples.extend_from_slice(&frame.to_transcription_format().samples);
        }

        // Timings for a recording where the speakers alternate every five seconds.
        let input = transcript(&[(0, 5_000), (5_000, 10_000), (10_000, 15_000)]);
        let output = diarizer.diarize(&input, &samples, 16_000).expect("diarize");

        let speakers: std::collections::HashSet<_> = output
            .segments
            .iter()
            .filter_map(|s| s.speaker.clone())
            .collect();

        println!("separated into {speakers:?}");
        assert_eq!(speakers.len(), 2, "expected two voices, got {speakers:?}");
    }
}
