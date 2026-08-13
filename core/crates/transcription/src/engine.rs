//! Transcription engines.

use async_trait::async_trait;

use notewise_audio_capture::{AudioFrame, AudioFormat};

use crate::models::{ModelInfo, ModelStore};
use crate::segment::{Segment, Transcript};
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
/// # Windowing
///
/// whisper.cpp transcribes a buffer, not a stream. Feeding it one 100 ms frame at a time
/// would produce nonsense — the model needs enough context to resolve a phrase. Audio is
/// therefore accumulated and decoded a window at a time, with segment timings offset back
/// into the meeting's own time base so the transcript lines up with the recording.
pub struct WhisperEngine {
    model: ModelInfo,
    #[allow(dead_code)] // Only read by the feature-gated inference path.
    store: ModelStore,

    /// Audio accumulated since the last decode.
    buffer: Vec<f32>,
    /// Milliseconds of audio already decoded, for offsetting segment timings.
    offset_ms: i64,
    /// How much audio to accumulate before decoding.
    #[allow(dead_code)] // Only read by the feature-gated inference path.
    window_samples: usize,

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
            .field("buffered_samples", &self.buffer.len())
            .field("offset_ms", &self.offset_ms)
            .field("gpu", &Self::gpu_enabled())
            .finish()
    }
}

impl WhisperEngine {
    /// Decode window, in seconds.
    ///
    /// A tradeoff between latency to the first visible text and accuracy: shorter windows
    /// show text sooner but cut phrases in half, which the model then mis-transcribes.
    /// Ten seconds is comfortably longer than most sentences.
    pub const WINDOW_SECONDS: usize = 10;

    /// Create an engine for a model.
    ///
    /// Verifies the model is present and intact before claiming the engine is usable, so a
    /// missing model surfaces here rather than at the first frame of a live meeting.
    pub fn new(model: ModelInfo, store: ModelStore) -> Result<Self> {
        store.verify(&model)?;

        let window_samples =
            AudioFormat::transcription().sample_rate.hz() as usize * Self::WINDOW_SECONDS;

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
            buffer: Vec::with_capacity(window_samples),
            offset_ms: 0,
            window_samples,
            #[cfg(feature = "whisper")]
            context,
        })
    }

    pub fn model(&self) -> &ModelInfo {
        &self.model
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

    /// Decode whatever is buffered and return its segments.
    #[cfg(feature = "whisper")]
    fn decode(&mut self) -> Result<Vec<Segment>> {
        if self.buffer.is_empty() {
            return Ok(Vec::new());
        }

        let audio = std::mem::take(&mut self.buffer);
        let rate = AudioFormat::transcription().sample_rate.hz() as i64;
        let duration_ms = (audio.len() as i64 * 1000) / rate;

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

            // whisper.cpp reports centiseconds relative to this window; shift into the
            // meeting's time base so the transcript lines up with the recording.
            let start = state.full_get_segment_t0(i).unwrap_or(0) * 10 + self.offset_ms;
            let end = state.full_get_segment_t1(i).unwrap_or(0) * 10 + self.offset_ms;

            let trimmed = text.trim();
            if !trimmed.is_empty() {
                segments.push(Segment::new(trimmed, start, end));
            }
        }

        self.offset_ms += duration_ms;
        Ok(segments)
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

        self.buffer.extend_from_slice(&frame.samples);

        if self.buffer.len() >= self.window_samples {
            return self.decode();
        }
        Ok(Vec::new())
    }

    #[cfg(not(feature = "whisper"))]
    async fn feed(&mut self, _frame: &AudioFrame) -> Result<Vec<Segment>> {
        // Fails loudly. Returning an empty vec would look like "no speech detected" and
        // produce an empty transcript with no indication anything was wrong.
        Err(Self::unavailable())
    }

    #[cfg(feature = "whisper")]
    async fn finish(&mut self) -> Result<Vec<Segment>> {
        self.decode()
    }

    #[cfg(not(feature = "whisper"))]
    async fn finish(&mut self) -> Result<Vec<Segment>> {
        Err(Self::unavailable())
    }
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
            TranscriptionError::EngineUnavailable { engine: "whisper", .. }
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
        let transcript = engine.transcribe_all(&mut source).await.expect("transcribe");

        let text = transcript.to_text().to_lowercase();
        println!("\n--- transcript ---\n{}\n", transcript.to_text());
        println!("gpu enabled: {}", WhisperEngine::gpu_enabled());

        assert!(text.contains("postgres"), "got: {text}");
        assert!(text.contains("friday"), "got: {text}");

        // Timings must land inside the recording, not at zero or past its end.
        let first = transcript.segments.first().expect("at least one segment");
        assert!(first.end_ms > first.start_ms);
        assert!(transcript.span_ms() > 1000, "span was {}", transcript.span_ms());
    }

    #[tokio::test]
    async fn engines_are_usable_behind_a_trait_object() {
        let mut engine: Box<dyn TranscriptionEngine> = Box::new(MockEngine::new());
        let mut source = tone_source(200);

        assert!(!engine.transcribe_all(&mut source).await.unwrap().is_empty());
        assert_eq!(engine.name(), "mock");
    }
}
