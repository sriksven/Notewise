//! Transcription engines.

use async_trait::async_trait;

use notewise_audio_capture::{AudioFormat, AudioFrame, Vad};

use crate::models::{ModelInfo, ModelStore};
use crate::segment::{Segment, Transcript};
use crate::stream::UtteranceBuffer;
// Only named by the feature-gated inference path.
#[cfg(feature = "whisper")]
use crate::stream::Utterance;
use crate::{Result, TranscriptionError};

/// A speech recognition engine.
///
/// Engines are fed audio frames and produce segments. The interface is streaming-shaped —
/// `feed` then `finish` — because a meeting needs a transcript appearing as it runs, not
/// only after it ends.
#[async_trait]
pub trait TranscriptionEngine: std::fmt::Debug + Send {
    fn name(&self) -> &str;

    /// The audio format this engine requires. Callers convert before feeding.
    fn required_format(&self) -> AudioFormat {
        AudioFormat::transcription()
    }

    /// Feed one frame, returning any segments completed by it.
    ///
    /// Usually empty — engines buffer until they have enough audio to decode a phrase.
    async fn feed(&mut self, frame: &AudioFrame) -> Result<Vec<Segment>>;

    /// Flush buffered audio and return the remaining segments.
    async fn finish(&mut self) -> Result<Vec<Segment>>;

    /// Transcribe a complete source in one pass.
    ///
    /// Provided rather than required: every engine can be driven this way, and the import
    /// path uses it.
    async fn transcribe_all(
        &mut self,
        source: &mut dyn notewise_audio_capture::AudioSource,
    ) -> Result<Transcript> {
        let mut segments = Vec::new();

        while let Some(frame) = source
            .next_frame()
            .map_err(|e| TranscriptionError::BadAudio(e.to_string()))?
        {
            let ready = if frame.format == self.required_format() {
                self.feed(&frame).await?
            } else {
                self.feed(&frame.to_transcription_format()).await?
            };
            segments.extend(ready);
        }

        segments.extend(self.finish().await?);
        Ok(Transcript::new(segments).normalized())
    }
}

/// Whisper.cpp.
///
/// Real inference, behind the `whisper` feature. Enabling it pulls in a cmake build of
/// whisper.cpp; add `whisper-metal`, `whisper-cuda`, or `whisper-vulkan` for GPU
/// acceleration. Without the feature the type still exists and every method returns
/// [`TranscriptionError::EngineUnavailable`], so callers compile either way.
///
/// # Segmentation
///
/// whisper.cpp transcribes a buffer, not a stream. Feeding it one 100 ms frame at a time
/// would produce nonsense — the model needs enough context to resolve a phrase — so audio has
/// to be grouped before it is decoded.
///
/// Grouping is delegated to [`UtteranceBuffer`], which cuts on the *speaker pausing* rather
/// than on a clock. That module's docs explain what a fixed window got wrong; the short
/// version is that a ten-second window meant a ten-second lag, decoded room tone the model
/// then invented text for, and segment timings that bore no relation to when things were
/// said. Timings here are offset back into the meeting's own time base, so the transcript
/// lines up with the recording.
pub struct WhisperEngine {
    model: ModelInfo,
    #[allow(dead_code)] // Only read by the feature-gated inference path.
    store: ModelStore,

    /// Groups incoming frames into buffers worth decoding.
    stream: UtteranceBuffer,

    /// Spoken language, or `None` to let Whisper detect it.
    ///
    /// Detection costs a pass over the first buffer and is occasionally wrong — a meeting that
    /// opens in English and is detected as Welsh transcribes as nonsense for its whole length.
    /// Naming the language removes that failure entirely, so a picker is worth having.
    #[allow(dead_code)]
    language: Option<String>,

    #[cfg(feature = "whisper")]
    context: whisper_rs::WhisperContext,
}

// `whisper_rs::WhisperContext` is not `Debug`, so this is written by hand. It reports the
// model and buffer state — the loaded weights have nothing useful to print, and audio must
// not end up in a log line.
impl std::fmt::Debug for WhisperEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhisperEngine")
            .field("model", &self.model.name)
            .field("buffered_ms", &self.stream.buffered_ms())
            .field("offset_ms", &self.stream.offset_ms())
            .field("gpu", &Self::gpu_enabled())
            .finish()
    }
}

impl WhisperEngine {
    /// Create an engine for a model.
    ///
    /// Verifies the model is present and intact before claiming the engine is usable, so a
    /// missing model surfaces here rather than at the first frame of a live meeting.
    pub fn new(model: ModelInfo, store: ModelStore) -> Result<Self> {
        store.verify(&model)?;

        #[cfg(feature = "whisper")]
        let context = {
            let path = store.path_for(&model);
            whisper_rs::WhisperContext::new_with_params(
                &path.to_string_lossy(),
                whisper_rs::WhisperContextParameters::default(),
            )
            .map_err(|e| TranscriptionError::Download(format!("loading {}: {e}", path.display())))?
        };

        Ok(Self {
            model,
            store,
            stream: UtteranceBuffer::new(AudioFormat::transcription().sample_rate.hz()),
            language: None,
            #[cfg(feature = "whisper")]
            context,
        })
    }

    pub fn model(&self) -> &ModelInfo {
        &self.model
    }

    /// Set the spoken language, e.g. `en`. `None` asks Whisper to detect it.
    ///
    /// English-only models (`*.en`) reject any language but English, so this is ignored for
    /// them rather than passed through to fail at the first buffer.
    pub fn with_language(mut self, language: Option<String>) -> Self {
        self.language = language.filter(|_| !self.model.name.ends_with(".en"));
        self
    }

    /// Tune the speech gate.
    pub fn with_vad(mut self, vad: Vad) -> Self {
        self.stream.set_vad(vad);
        self
    }

    /// Decode every buffer, including ones containing no speech.
    ///
    /// Off by default, and turning it on brings the hallucinations back. It exists for
    /// diagnosing the gate itself: if a transcript is missing words, running once ungated
    /// answers whether the gate or the model dropped them.
    pub fn without_speech_gate(mut self) -> Self {
        self.stream.disable_speech_gate();
        self
    }

    /// The noise floor the gate has settled on, in dBFS.
    pub fn noise_floor_db(&self) -> f32 {
        self.stream.noise_floor_db()
    }

    /// Whether GPU acceleration was compiled in.
    pub fn gpu_enabled() -> bool {
        cfg!(any(
            feature = "whisper-metal",
            feature = "whisper-cuda",
            feature = "whisper-vulkan"
        ))
    }

    #[cfg(not(feature = "whisper"))]
    fn unavailable() -> TranscriptionError {
        TranscriptionError::EngineUnavailable {
            engine: "whisper",
            reason: "built without the 'whisper' feature",
        }
    }

    /// Decode one utterance.
    ///
    /// The buffer arrives already gated: [`UtteranceBuffer`] guarantees it holds real speech,
    /// so there is no silence check here. That gate is not optional politeness — fed room
    /// tone, Whisper does not return nothing, it invents. Ten seconds of it in a live test
    /// here produced "I know. You're happy to see a new work to grow.", indistinguishable in
    /// the transcript from something a person said.
    #[cfg(feature = "whisper")]
    fn decode(&mut self, utterance: Utterance) -> Result<Vec<Segment>> {
        if utterance.samples.is_empty() {
            return Ok(Vec::new());
        }

        let audio = utterance.samples;
        let offset_ms = utterance.offset_ms;

        let mut state = self.context.create_state().map_err(|e| {
            TranscriptionError::BadAudio(format!("could not create a decode state: {e}"))
        })?;

        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        // whisper.cpp writes to stdout by default, which would corrupt the MCP server's
        // JSON-RPC stream sharing this process.
        params.set_print_progress(false);
        params.set_print_special(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        // whisper.cpp's own defences against inventing text. The gate upstream removes buffers
        // with no speech in them; these catch the harder case of a buffer that has some speech
        // and some silence, where the model will happily fill the silence.
        //
        // Values are whisper.cpp's own defaults for these thresholds, which are *not* applied
        // unless set explicitly through this API.
        params.set_no_speech_thold(0.6);
        params.set_entropy_thold(2.4);
        params.set_logprob_thold(-1.0);
        // Suppress blank output at the start of a window, and non-speech tokens like
        // "(wind blowing)" — subtitle-corpus artefacts that are not meeting content.
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);

        // Set before inference, and only for multilingual models.
        if let Some(language) = &self.language {
            params.set_language(Some(language));
        }

        state
            .full(params, &audio)
            .map_err(|e| TranscriptionError::BadAudio(format!("inference failed: {e}")))?;

        let count = state
            .full_n_segments()
            .map_err(|e| TranscriptionError::BadAudio(format!("reading segments: {e}")))?;

        let mut segments = Vec::with_capacity(count as usize);
        for i in 0..count {
            let text = state
                .full_get_segment_text(i)
                .map_err(|e| TranscriptionError::BadAudio(format!("reading segment {i}: {e}")))?;

            // whisper.cpp reports centiseconds relative to this buffer; shift into the
            // meeting's time base so the transcript lines up with the recording.
            let start = state.full_get_segment_t0(i).unwrap_or(0) * 10 + offset_ms;
            let end = state.full_get_segment_t1(i).unwrap_or(0) * 10 + offset_ms;

            let trimmed = text.trim();
            if !trimmed.is_empty() && !is_non_speech_marker(trimmed) {
                segments.push(Segment::new(trimmed, start, end));
            }
        }

        Ok(collapse_repetitions(segments))
    }
}

#[async_trait]
impl TranscriptionEngine for WhisperEngine {
    fn name(&self) -> &str {
        &self.model.name
    }

    #[cfg(feature = "whisper")]
    async fn feed(&mut self, frame: &AudioFrame) -> Result<Vec<Segment>> {
        if frame.format != self.required_format() {
            return Err(TranscriptionError::BadAudio(format!(
                "expected {}, got {}",
                self.required_format(),
                frame.format
            )));
        }

        match self.stream.push(&frame.samples) {
            Some(utterance) => self.decode(utterance),
            None => Ok(Vec::new()),
        }
    }

    #[cfg(not(feature = "whisper"))]
    async fn feed(&mut self, _frame: &AudioFrame) -> Result<Vec<Segment>> {
        // Fails loudly. Returning an empty vec would look like "no speech detected" and
        // produce an empty transcript with no indication anything was wrong.
        Err(Self::unavailable())
    }

    #[cfg(feature = "whisper")]
    async fn finish(&mut self) -> Result<Vec<Segment>> {
        match self.stream.flush() {
            Some(utterance) => self.decode(utterance),
            None => Ok(Vec::new()),
        }
    }

    #[cfg(not(feature = "whisper"))]
    async fn finish(&mut self) -> Result<Vec<Segment>> {
        Err(Self::unavailable())
    }
}

/// Whether whisper.cpp emitted a marker rather than transcribed speech.
///
/// whisper.cpp signals "there was nothing here" in-band, as text: `[BLANK_AUDIO]`, and its
/// training corpus of subtitles teaches it to emit annotations like `(silence)` or
/// `[MUSIC PLAYING]`. Stored unfiltered these become transcript lines that read as things
/// someone said — observed here as a `[BLANK_AUDIO]` segment spanning ten seconds of a real
/// recording's opening room tone.
///
/// Matching is deliberately narrow: the whole segment must be one bracketed or parenthesised
/// annotation. A real utterance that merely *contains* "(laughs)" keeps its words.
// Called only from the feature-gated inference path; its tests are not gated.
#[cfg_attr(not(feature = "whisper"), allow(dead_code))]
fn is_non_speech_marker(text: &str) -> bool {
    let bounded = matches!(
        (text.chars().next(), text.chars().next_back()),
        (Some('['), Some(']')) | (Some('('), Some(')')) | (Some('*'), Some('*'))
    );
    if !bounded {
        return false;
    }

    // A bracketed span containing more than a few words is more likely a real transcription
    // of someone reading a list than an annotation.
    let inner = text[1..text.len().saturating_sub(1)]
        .trim()
        .to_ascii_lowercase();
    if inner.split_whitespace().count() > 4 {
        return false;
    }

    const MARKERS: [&str; 12] = [
        "blank_audio",
        "blank audio",
        "silence",
        "silent",
        "no speech",
        "inaudible",
        "music",
        "music playing",
        "sound",
        "noise",
        "applause",
        "background noise",
    ];
    MARKERS.contains(&inner.as_str())
}

/// Collapse a decoder repetition loop into the one thing that was probably said.
///
/// Whisper, given a buffer it cannot resolve, sometimes emits the same short phrase over and
/// over until the buffer runs out. Observed in this repository: five seconds of room tone at
/// the end of a recording became `Okay.` four times, one per second, stored as four transcript
/// lines. The speech gate upstream is the primary defence and removes the buffers that cause
/// this; this is the second line, for a buffer that holds real speech and loops anyway.
///
/// A run of three or more *adjacent, identical* segments collapses to one spanning the run.
/// Three is the threshold because two is ordinary speech — "No. No." is a thing people say —
/// while a phrase repeated verbatim three times in consecutive segments, with no other words
/// between them, is the decoder rather than the speaker.
// Called only from the feature-gated inference path; its tests are not gated.
#[cfg_attr(not(feature = "whisper"), allow(dead_code))]
fn collapse_repetitions(segments: Vec<Segment>) -> Vec<Segment> {
    /// Adjacent segments must repeat at least this many times to count as a loop.
    const RUN: usize = 3;

    if segments.len() < RUN {
        return segments;
    }

    let key = |segment: &Segment| segment.text.trim().to_lowercase();

    let mut out: Vec<Segment> = Vec::with_capacity(segments.len());
    let mut index = 0;

    while index < segments.len() {
        let text = key(&segments[index]);
        let mut end = index + 1;
        while end < segments.len() && key(&segments[end]) == text {
            end += 1;
        }

        let run = end - index;
        if run >= RUN {
            // Keep one occurrence, spanning the whole run: something was said in that audio,
            // and claiming only the first second of it would be its own small lie.
            let mut collapsed = segments[index].clone();
            collapsed.end_ms = segments[end - 1].end_ms;
            tracing::debug!(
                text = %collapsed.text,
                repeats = run,
                "collapsed a decoder repetition loop"
            );
            out.push(collapsed);
        } else {
            out.extend_from_slice(&segments[index..end]);
        }

        index = end;
    }

    out
}

/// A deterministic engine that emits a segment per frame of non-silent audio.
///
/// Real, and the reason the pipeline above can be tested end to end without a model download,
/// a GPU, or a cmake toolchain.
#[derive(Debug, Default)]
pub struct MockEngine {
    frames_seen: usize,
    pending: Vec<Segment>,
    /// Frames below this RMS are treated as silence and produce no segment.
    silence_threshold: f32,
}

impl MockEngine {
    pub fn new() -> Self {
        Self {
            silence_threshold: 0.01,
            ..Default::default()
        }
    }

    /// Number of frames fed so far. Lets tests assert the pipeline delivered audio.
    pub fn frames_seen(&self) -> usize {
        self.frames_seen
    }
}

#[async_trait]
impl TranscriptionEngine for MockEngine {
    fn name(&self) -> &str {
        "mock"
    }

    async fn feed(&mut self, frame: &AudioFrame) -> Result<Vec<Segment>> {
        if frame.format != self.required_format() {
            return Err(TranscriptionError::BadAudio(format!(
                "expected {}, got {}",
                self.required_format(),
                frame.format
            )));
        }

        self.frames_seen += 1;

        // Silence produces nothing, mirroring a real engine and letting tests exercise the
        // "meeting with a long pause" path.
        if notewise_audio_capture::rms(&frame.samples) < self.silence_threshold {
            return Ok(Vec::new());
        }

        self.pending.push(
            Segment::new(
                format!("Mock segment {}", self.frames_seen),
                frame.timestamp_ms,
                frame.timestamp_ms + frame.duration_ms(),
            )
            .with_confidence(0.95),
        );

        // Emit in pairs, so tests exercise both the buffering and flushing paths.
        if self.pending.len() >= 2 {
            return Ok(std::mem::take(&mut self.pending));
        }
        Ok(Vec::new())
    }

    async fn finish(&mut self) -> Result<Vec<Segment>> {
        Ok(std::mem::take(&mut self.pending))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notewise_audio_capture::{CaptureConfig, SyntheticSource, Waveform};

    fn tone_source(duration_ms: u32) -> SyntheticSource {
        SyntheticSource::new(
            Waveform::Sine { hz: 440 },
            duration_ms,
            &CaptureConfig::default(),
        )
    }

    #[tokio::test]
    async fn the_mock_engine_transcribes_a_whole_source() {
        let mut engine = MockEngine::new();
        let mut source = tone_source(1000);

        let transcript = engine.transcribe_all(&mut source).await.unwrap();

        assert_eq!(engine.frames_seen(), 10);
        assert_eq!(transcript.segments.len(), 10, "one segment per audio frame");
    }

    #[tokio::test]
    async fn buffered_segments_are_flushed_by_finish() {
        // An odd frame count leaves one segment buffered; losing it would truncate the
        // last thing anyone said.
        let mut engine = MockEngine::new();
        let mut source = tone_source(300);

        let transcript = engine.transcribe_all(&mut source).await.unwrap();
        assert_eq!(transcript.segments.len(), 3);
    }

    #[tokio::test]
    async fn silence_produces_no_segments() {
        let mut engine = MockEngine::new();
        let mut source = SyntheticSource::silence();

        let transcript = engine.transcribe_all(&mut source).await.unwrap();

        assert!(transcript.is_empty());
        assert_eq!(engine.frames_seen(), 10, "silence is still consumed");
    }

    #[tokio::test]
    async fn segments_are_chronological_and_non_overlapping() {
        let mut engine = MockEngine::new();
        let mut source = tone_source(1000);
        let transcript = engine.transcribe_all(&mut source).await.unwrap();

        for pair in transcript.segments.windows(2) {
            assert!(pair[0].start_ms <= pair[1].start_ms);
            assert!(
                !pair[0].overlaps(&pair[1]),
                "{:?} overlaps {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[tokio::test]
    async fn segment_timings_line_up_with_the_source() {
        let mut engine = MockEngine::new();
        let mut source = tone_source(1000);
        let transcript = engine.transcribe_all(&mut source).await.unwrap();

        assert_eq!(transcript.segments.first().unwrap().start_ms, 0);
        assert_eq!(transcript.segments.last().unwrap().end_ms, 1000);
        assert_eq!(transcript.span_ms(), 1000);
    }

    #[tokio::test]
    async fn mismatched_audio_format_is_rejected() {
        let mut engine = MockEngine::new();
        let frame = AudioFrame::new(
            vec![0.5; 100],
            AudioFormat::new(notewise_audio_capture::SampleRate::STUDIO, 2),
            0,
        );

        assert!(matches!(
            engine.feed(&frame).await.unwrap_err(),
            TranscriptionError::BadAudio(_)
        ));
    }

    #[tokio::test]
    async fn transcribe_all_converts_mismatched_audio_rather_than_failing() {
        // A 48 kHz stereo source is what a real system tap produces; the pipeline must
        // handle it without the caller converting first.
        let mut engine = MockEngine::new();
        let mut source = notewise_audio_capture::FileSource::from_samples(
            vec![0.5; 9600],
            AudioFormat::new(notewise_audio_capture::SampleRate::STUDIO, 2),
            100,
        );

        let transcript = engine.transcribe_all(&mut source).await.unwrap();
        assert!(!transcript.is_empty());
    }

    #[tokio::test]
    #[cfg(not(feature = "whisper"))]
    async fn whisper_fails_loudly_rather_than_returning_an_empty_transcript() {
        // An empty transcript with no error looks like "the meeting had no speech".
        let dir = std::env::temp_dir().join(format!("notewise-whisper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = ModelStore::new(&dir);

        let model = ModelInfo {
            name: "stub".into(),
            size: crate::ModelSize::Tiny,
            url: "https://example.invalid/ggml-stub.bin".into(),
            bytes: 4,
            multilingual: false,
        };
        std::fs::write(store.path_for(&model), b"abcd").unwrap();

        let mut engine = WhisperEngine::new(model, store).expect("model verifies");
        let frame = AudioFrame::new(vec![0.5; 1600], AudioFormat::transcription(), 0);

        let err = engine.feed(&frame).await.expect_err("must not pretend");
        assert!(matches!(
            err,
            TranscriptionError::EngineUnavailable {
                engine: "whisper",
                ..
            }
        ));
    }

    #[test]
    fn gpu_support_is_reported_from_the_enabled_features() {
        // Lets the UI say whether inference is accelerated without probing the device.
        assert_eq!(
            WhisperEngine::gpu_enabled(),
            cfg!(any(
                feature = "whisper-metal",
                feature = "whisper-cuda",
                feature = "whisper-vulkan"
            ))
        );
    }

    #[test]
    fn whisper_refuses_to_construct_without_its_model() {
        let store = ModelStore::new(std::env::temp_dir().join("notewise-absent"));
        let err = WhisperEngine::new(crate::ModelRegistry::default_model(), store)
            .expect_err("should refuse");

        assert!(
            matches!(err, TranscriptionError::ModelNotDownloaded { .. }),
            "a missing model must surface before a meeting starts, not during one"
        );
    }

    /// End-to-end inference against the real model.
    ///
    /// Ignored because it needs a downloaded model; run with
    /// `NOTEWISE_MODEL_DIR=... cargo test -p notewise-transcription \
    ///   --features whisper-metal -- --ignored --nocapture`
    /// whisper.cpp signals "nothing here" as text. Stored unfiltered, `[BLANK_AUDIO]` became
    /// a ten-second transcript line in a real recording.
    #[test]
    fn whisper_non_speech_markers_are_not_transcript() {
        for marker in [
            "[BLANK_AUDIO]",
            "[ Silence ]",
            "(silence)",
            "[MUSIC PLAYING]",
            "(applause)",
            "[INAUDIBLE]",
            "*music*",
            "[ background noise ]",
        ] {
            assert!(is_non_speech_marker(marker), "{marker} should be filtered");
        }
    }

    /// The exact output of the bug this filter was written for: the tail of a real recording,
    /// four identical one-second segments where a quiet room had been.
    #[test]
    fn a_decoder_repetition_loop_collapses_to_one_segment() {
        let looped = vec![
            Segment::new("Okay.", 10_000, 11_000),
            Segment::new("Okay.", 11_000, 12_000),
            Segment::new("Okay.", 12_000, 13_000),
            Segment::new("Okay.", 13_000, 14_000),
        ];

        let out = collapse_repetitions(looped);
        assert_eq!(out.len(), 1, "got {:?}", out);
        assert_eq!(out[0].text, "Okay.");
        // Spanning the whole run, not just the first second of it.
        assert_eq!((out[0].start_ms, out[0].end_ms), (10_000, 14_000));
    }

    /// Repetition is a thing people genuinely do. Two in a row is speech, not a loop.
    #[test]
    fn ordinary_repetition_is_left_alone() {
        let real = vec![
            Segment::new("No.", 0, 500),
            Segment::new("No.", 500, 1_000),
            Segment::new("I disagree.", 1_000, 2_000),
        ];

        assert_eq!(collapse_repetitions(real.clone()), real);
    }

    /// A loop must not swallow the words around it.
    #[test]
    fn a_loop_between_real_speech_keeps_the_real_speech() {
        let out = collapse_repetitions(vec![
            Segment::new("Let us start.", 0, 1_000),
            Segment::new("Yeah.", 1_000, 2_000),
            Segment::new("yeah.", 2_000, 3_000),
            Segment::new("Yeah. ", 3_000, 4_000),
            Segment::new("So the migration lands Friday.", 4_000, 6_000),
        ]);

        let texts: Vec<_> = out.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["Let us start.", "Yeah.", "So the migration lands Friday."],
            "case and trailing space must not hide a loop, and real words must survive"
        );
    }

    #[test]
    fn collapsing_is_a_no_op_on_short_or_empty_output() {
        assert!(collapse_repetitions(Vec::new()).is_empty());

        let two = vec![
            Segment::new("Okay.", 0, 500),
            Segment::new("Okay.", 500, 1_000),
        ];
        assert_eq!(collapse_repetitions(two.clone()), two);
    }

    /// The filter must not eat speech. A bracketed aside inside a real sentence, or a long
    /// bracketed span, is far more likely to be something a person actually said.
    #[test]
    fn real_speech_is_not_mistaken_for_a_marker() {
        for text in [
            "We agreed to ship on Friday.",
            "The silence in that meeting was telling.",
            "(I think we should revisit the pricing model next quarter)",
            "[the numbers are in the appendix at the back]",
            "music",
            "[silence]-ish",
        ] {
            assert!(!is_non_speech_marker(text), "{text} should be kept");
        }
    }

    /// The regression test for the hallucination this gate exists to stop.
    ///
    /// Ungated, real Whisper on room tone returns invented sentences — a live recording in
    /// this repository produced *"I know. You're happy to see a new work to grow."* over the
    /// silence before anyone spoke. Gated, it must return nothing at all.
    ///
    /// Runs both ways in one test so a pass genuinely means the gate did the work, rather
    /// than the model happening to stay quiet on this particular noise.
    ///
    /// `NOTEWISE_MODEL_DIR=... cargo test -p notewise-transcription \
    ///   --features whisper-metal -- --ignored --nocapture`
    #[tokio::test]
    #[cfg(feature = "whisper")]
    #[ignore = "requires a downloaded model"]
    async fn the_speech_gate_stops_whisper_inventing_words_over_room_tone() {
        let dir = std::env::var("NOTEWISE_MODEL_DIR").expect("NOTEWISE_MODEL_DIR");
        let model = crate::ModelRegistry::default_model();
        let format = AudioFormat::transcription();
        let rate = format.sample_rate.hz() as usize;

        // Prefers a real recording, because synthetic noise is too clean to reproduce this:
        // set NOTEWISE_ROOMTONE_WAV to a few seconds of a genuinely quiet room. Falls back to
        // synthesis so the test is still runnable without one.
        let frame = match std::env::var("NOTEWISE_ROOMTONE_WAV") {
            Ok(wav) => {
                use notewise_audio_capture::AudioSource;
                let mut source = notewise_audio_capture::FileSource::open_wav(&wav).expect("wav");
                let mut samples = Vec::new();
                while let Some(f) = source.next_frame().expect("frame") {
                    let f = if f.format == format {
                        f
                    } else {
                        f.to_transcription_format()
                    };
                    samples.extend_from_slice(&f.samples);
                }
                println!("real room tone: {} samples from {wav}", samples.len());
                AudioFrame::new(samples, format, 0)
            }
            Err(_) => {
                let mut state = 0x2545_F491_4F6C_DD1Du64;
                let samples: Vec<f32> = (0..rate * 12)
                    .map(|_| {
                        state = state
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        (((state >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0) * 0.002
                    })
                    .collect();
                println!("synthetic room tone: {} samples", samples.len());
                AudioFrame::new(samples, format, 0)
            }
        };

        let mut ungated = WhisperEngine::new(model.clone(), ModelStore::new(&dir))
            .expect("engine")
            .without_speech_gate();
        ungated.feed(&frame).await.expect("feed");
        let invented = ungated.finish().await.expect("finish");
        println!("ungated on room tone -> {invented:?}");

        let mut gated = WhisperEngine::new(model, ModelStore::new(&dir)).expect("engine");
        gated.feed(&frame).await.expect("feed");
        let gated_out = gated.finish().await.expect("finish");
        println!("gated on room tone   -> {gated_out:?}");
        println!("noise floor: {:.1} dBFS", gated.noise_floor_db());

        assert!(
            gated_out.is_empty(),
            "the gate let {} segment(s) through on room tone: {:?}",
            gated_out.len(),
            gated_out.iter().map(|s| &s.text).collect::<Vec<_>>()
        );

        // Not asserted as a hard requirement — whisper.cpp's own thresholds may also catch
        // this noise — but printed, because if the ungated run is empty too then this test
        // proves nothing and the sample needs to change.
        if invented.is_empty() {
            println!("NOTE: ungated run was also empty; this sample does not exercise the gate");
        }
    }

    #[tokio::test]
    #[cfg(feature = "whisper")]
    #[ignore = "requires a downloaded model and a speech sample"]
    async fn transcribes_real_speech() {
        let dir = std::env::var("NOTEWISE_MODEL_DIR").expect("NOTEWISE_MODEL_DIR");
        let sample = std::env::var("NOTEWISE_SAMPLE_WAV").expect("NOTEWISE_SAMPLE_WAV");

        let store = ModelStore::new(&dir);
        let model = crate::ModelRegistry::default_model();
        let mut engine = WhisperEngine::new(model, store).expect("engine");

        let mut source = notewise_audio_capture::FileSource::open_wav(&sample).expect("wav");
        let transcript = engine
            .transcribe_all(&mut source)
            .await
            .expect("transcribe");

        let text = transcript.to_text().to_lowercase();
        println!("\n--- transcript ---\n{}\n", transcript.to_text());
        println!("gpu enabled: {}", WhisperEngine::gpu_enabled());

        assert!(text.contains("postgres"), "got: {text}");
        assert!(text.contains("friday"), "got: {text}");

        // Timings must land inside the recording, not at zero or past its end.
        let first = transcript.segments.first().expect("at least one segment");
        assert!(first.end_ms > first.start_ms);
        assert!(
            transcript.span_ms() > 1000,
            "span was {}",
            transcript.span_ms()
        );
    }

    #[tokio::test]
    async fn engines_are_usable_behind_a_trait_object() {
        let mut engine: Box<dyn TranscriptionEngine> = Box::new(MockEngine::new());
        let mut source = tone_source(200);

        assert!(!engine.transcribe_all(&mut source).await.unwrap().is_empty());
        assert_eq!(engine.name(), "mock");
    }
}
