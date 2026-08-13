use serde::{Deserialize, Serialize};

/// One timestamped chunk of recognized speech.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
    /// Engine confidence in `0.0..=1.0`, when the engine reports one.
    pub confidence: Option<f32>,
    /// Speaker label. Always `None` here — populated later by `diarization`, which is a
    /// separate crate so speaker quality can be iterated on without touching this path.
    pub speaker: Option<String>,
}

impl Segment {
    pub fn new(text: impl Into<String>, start_ms: i64, end_ms: i64) -> Self {
        Self {
            text: text.into(),
            start_ms,
            end_ms,
            confidence: None,
            speaker: None,
        }
    }

    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = Some(confidence.clamp(0.0, 1.0));
        self
    }

    pub fn duration_ms(&self) -> i64 {
        (self.end_ms - self.start_ms).max(0)
    }

    /// Whether this segment carries recognizable speech.
    ///
    /// Engines emit empty or whitespace-only segments for silence and non-speech noise;
    /// storing them clutters the transcript and skews diarization timing.
    pub fn is_speech(&self) -> bool {
        !self.text.trim().is_empty()
    }

    /// Whether two segments overlap in time. Used to detect misordered engine output.
    pub fn overlaps(&self, other: &Segment) -> bool {
        self.start_ms < other.end_ms && other.start_ms < self.end_ms
    }
}

/// An ordered set of segments.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub segments: Vec<Segment>,
}

impl Transcript {
    pub fn new(segments: Vec<Segment>) -> Self {
        Self { segments }
    }

    /// Drop non-speech segments and sort by start time.
    ///
    /// Engines can emit segments slightly out of order when processing overlapping windows;
    /// everything downstream assumes chronological order.
    pub fn normalized(mut self) -> Self {
        self.segments.retain(Segment::is_speech);
        self.segments.sort_by_key(|s| s.start_ms);
        self
    }

    /// Plain text, one line per segment, speaker-prefixed where known.
    pub fn to_text(&self) -> String {
        self.segments
            .iter()
            .map(|s| match &s.speaker {
                Some(speaker) => format!("{speaker}: {}", s.text),
                None => s.text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Total speech duration, excluding gaps.
    pub fn speech_duration_ms(&self) -> i64 {
        self.segments.iter().map(Segment::duration_ms).sum()
    }

    /// Wall-clock span from the first segment's start to the last one's end.
    pub fn span_ms(&self) -> i64 {
        match (self.segments.first(), self.segments.last()) {
            (Some(first), Some(last)) => (last.end_ms - first.start_ms).max(0),
            _ => 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_is_never_negative() {
        // Guards against an engine emitting a reversed range.
        assert_eq!(Segment::new("x", 5000, 1000).duration_ms(), 0);
        assert_eq!(Segment::new("x", 1000, 5000).duration_ms(), 4000);
    }

    #[test]
    fn confidence_is_clamped_to_a_valid_range() {
        assert_eq!(Segment::new("x", 0, 1).with_confidence(1.5).confidence, Some(1.0));
        assert_eq!(Segment::new("x", 0, 1).with_confidence(-0.5).confidence, Some(0.0));
    }

    #[test]
    fn whitespace_only_segments_are_not_speech() {
        assert!(!Segment::new("", 0, 100).is_speech());
        assert!(!Segment::new("   \n ", 0, 100).is_speech());
        assert!(Segment::new("hello", 0, 100).is_speech());
    }

    #[test]
    fn overlap_detection_is_symmetric() {
        let a = Segment::new("a", 0, 1000);
        let b = Segment::new("b", 500, 1500);
        let c = Segment::new("c", 1000, 2000);

        assert!(a.overlaps(&b) && b.overlaps(&a));
        // Touching at a boundary is not overlapping.
        assert!(!a.overlaps(&c) && !c.overlaps(&a));
    }

    #[test]
    fn normalizing_sorts_and_drops_non_speech() {
        let transcript = Transcript::new(vec![
            Segment::new("third", 5000, 6000),
            Segment::new("  ", 1000, 2000),
            Segment::new("first", 0, 1000),
            Segment::new("second", 2000, 3000),
        ])
        .normalized();

        let texts: Vec<_> = transcript.segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["first", "second", "third"]);
    }

    #[test]
    fn normalizing_an_empty_transcript_is_safe() {
        assert!(Transcript::default().normalized().is_empty());
    }

    #[test]
    fn text_output_prefixes_only_known_speakers() {
        let mut attributed = Segment::new("Shipping Friday.", 0, 1000);
        attributed.speaker = Some("Alex".into());

        let transcript = Transcript::new(vec![attributed, Segment::new("Sounds good.", 1000, 2000)]);

        assert_eq!(transcript.to_text(), "Alex: Shipping Friday.\nSounds good.");
    }

    #[test]
    fn speech_duration_excludes_gaps_but_span_includes_them() {
        let transcript = Transcript::new(vec![
            Segment::new("a", 0, 1000),
            Segment::new("b", 9000, 10_000),
        ]);

        assert_eq!(transcript.speech_duration_ms(), 2000);
        assert_eq!(transcript.span_ms(), 10_000);
    }

    #[test]
    fn an_empty_transcript_has_no_span() {
        assert_eq!(Transcript::default().span_ms(), 0);
        assert_eq!(Transcript::default().speech_duration_ms(), 0);
    }

    #[test]
    fn transcripts_round_trip_through_json() {
        let transcript = Transcript::new(vec![Segment::new("hello", 0, 1000).with_confidence(0.9)]);
        let json = serde_json::to_string(&transcript).unwrap();

        assert_eq!(
            serde_json::from_str::<Transcript>(&json).unwrap(),
            transcript
        );
    }
}
