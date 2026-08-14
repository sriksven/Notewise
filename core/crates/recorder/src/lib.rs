//! The recording pipeline.
//!
//! Connects the pieces that until now existed only as separate, individually-tested
//! components: **capture → transcribe → attribute → store**.
//!
//! # Two ways to record, and why the second one is better
//!
//! [`ChannelPipeline`] keeps each capture source separate and labels every segment by the
//! source it arrived on. On a call that answers "who said this" exactly — the microphone is
//! you, the system tap is everyone else — with no model and no inference.
//!
//! ```text
//!   microphone ──→ TranscriptionEngine ──→ storage  ("You")
//!   system tap ──→ TranscriptionEngine ──→ storage  ("Others")
//!                  (sharing one loaded model)
//! ```
//!
//! [`Pipeline`] sums the sources into one stream and infers the speaker afterwards. It is what
//! to use when there is only one source, or when a caller genuinely wants a single mixed
//! track — but the inference is a guess, and summing is irreversible, so prefer the above.
//!
//! ```text
//!   AudioSource ──┐
//!                 ├─ Mixer ─→ TranscriptionEngine ─→ storage
//!   AudioSource ──┘                                     │
//!                                     Diarizer ─────────┘  (at stop)
//! ```
//!
//! # Two decisions worth knowing
//!
//! **Segments are stored as they arrive, unattributed, and diarized at the end.** Speaker
//! separation reads the gaps *between* segments, so it cannot label the current segment until
//! it knows what follows. Waiting for that would mean no transcript appears during the
//! meeting, which is the whole point of a live transcript. So text lands immediately and
//! speaker labels are filled in when recording stops.
//!
//! **The database lock is never held across inference.** A ten-second decode window with the
//! lock held would stall every read in the app, including the UI polling the transcript it is
//! waiting for.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

use std::time::Duration;

use thiserror::Error;

use notewise_audio_capture::{AudioSource, CaptureError, MixedSource, Mixer};
use notewise_diarization::{DiarizationError, Diarizer, SingleSpeakerDiarizer};
use notewise_graph::{EdgeKind, Graph, GraphError, NodeKind, NodeRef};
use notewise_storage::{Database, Id, MeetingRepository, NewTranscriptSegment, StorageError};
use notewise_transcription::{Segment, Transcript, TranscriptionEngine, TranscriptionError};

#[derive(Debug, Error)]
pub enum RecorderError {
    #[error(transparent)]
    Capture(#[from] CaptureError),

    #[error(transparent)]
    Transcription(#[from] TranscriptionError),

    #[error(transparent)]
    Diarization(#[from] DiarizationError),

    #[error(transparent)]
    Storage(#[from] StorageError),

    #[error(transparent)]
    Graph(#[from] GraphError),

    #[error("no audio source was attached")]
    NoInput,
}

pub type Result<T> = std::result::Result<T, RecorderError>;

/// What a recording produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordingStats {
    pub frames_processed: usize,
    pub segments_stored: usize,
    /// Segments that received a speaker label during the final pass.
    pub segments_attributed: usize,
    pub speakers_detected: usize,
    pub audio_ms: i64,
}

/// Pipeline tuning.
#[derive(Debug, Clone, Copy)]
pub struct PipelineConfig {
    /// Run speaker separation when recording stops.
    pub diarize: bool,
    /// How long to wait for a frame from a live source before checking for a stop signal.
    ///
    /// Only bounds shutdown latency — a silent room should not delay stopping by more than
    /// this, and a user who clicks stop expects it to stop.
    pub poll_timeout: Duration,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            diarize: true,
            poll_timeout: Duration::from_millis(250),
        }
    }
}

/// Runs one recording from audio to stored transcript.
#[derive(Debug)]
pub struct Pipeline {
    engine: Box<dyn TranscriptionEngine>,
    diarizer: Box<dyn Diarizer + Send>,
    config: PipelineConfig,
}

impl Pipeline {
    /// A pipeline with the default diarizer.
    ///
    /// The default is [`SingleSpeakerDiarizer`], not the pause heuristic. Gaps are not evidence
    /// about who was speaking, and treating them as such labelled one person's pause as a
    /// second participant in a real recording. Callers wanting the heuristic — an imported
    /// transcript of a call where people did alternate cleanly, say — opt in with
    /// [`Pipeline::with_diarizer`].
    pub fn new(engine: Box<dyn TranscriptionEngine>) -> Self {
        Self {
            engine,
            diarizer: Box::new(SingleSpeakerDiarizer),
            config: PipelineConfig::default(),
        }
    }

    pub fn with_diarizer(mut self, diarizer: Box<dyn Diarizer + Send>) -> Self {
        self.diarizer = diarizer;
        self
    }

    pub fn with_config(mut self, config: PipelineConfig) -> Self {
        self.config = config;
        self
    }

    /// Combine a microphone and a system tap into one source.
    ///
    /// Either may be absent, so a user without system-audio permission still records.
    pub fn mixed_source(
        mic: Option<Box<dyn AudioSource>>,
        system: Option<Box<dyn AudioSource>>,
    ) -> Result<MixedSource> {
        let source = MixedSource::new(mic, system, Mixer::default());
        if !source.has_input() {
            return Err(RecorderError::NoInput);
        }
        Ok(source)
    }

    /// Run a source to completion, storing segments as they are produced.
    ///
    /// `should_stop` is polled between frames so a live recording can be ended without
    /// waiting for the source to exhaust itself — a microphone never does.
    pub async fn run(
        &mut self,
        db: &Database,
        meeting_id: Id,
        source: &mut dyn AudioSource,
        mut should_stop: impl FnMut() -> bool,
    ) -> Result<RecordingStats> {
        let mut stats = RecordingStats::default();
        let required = self.engine.required_format();

        loop {
            if should_stop() {
                tracing::debug!("stop requested");
                break;
            }

            let Some(frame) = source.next_frame()? else {
                break;
            };

            stats.frames_processed += 1;
            stats.audio_ms += frame.duration_ms();

            // Convert once, here, so no engine has to care what the OS handed us.
            let ready = if frame.format == required {
                self.engine.feed(&frame).await?
            } else {
                self.engine.feed(&frame.to_transcription_format()).await?
            };

            // Store immediately: the point of a live transcript is that it is live.
            stats.segments_stored += self.store(db, meeting_id, &ready)?;
        }

        let remaining = self.engine.finish().await?;
        stats.segments_stored += self.store(db, meeting_id, &remaining)?;

        if self.config.diarize {
            let attributed = self.diarize(db, meeting_id)?;
            stats.segments_attributed = attributed.0;
            stats.speakers_detected = attributed.1;
        }

        tracing::info!(
            frames = stats.frames_processed,
            segments = stats.segments_stored,
            speakers = stats.speakers_detected,
            "recording finished"
        );

        Ok(stats)
    }

    /// Store a batch of segments.
    ///
    /// Takes the lock only for the write, and only when there is something to write.
    fn store(&self, db: &Database, meeting_id: Id, segments: &[Segment]) -> Result<usize> {
        // Left unattributed on purpose; filled in by the diarization pass.
        store_segments(db, meeting_id, segments, None)
    }

    /// Label stored segments with speakers.
    ///
    /// Runs over the whole transcript at once because turn-taking is only visible from the
    /// gaps between segments — which the live path cannot see for the segment it is storing.
    ///
    /// Returns `(segments labelled, distinct speakers)`.
    fn diarize(&self, db: &Database, meeting_id: Id) -> Result<(usize, usize)> {
        let repo = MeetingRepository::new(db);
        let stored = repo.segments(meeting_id)?;

        if stored.is_empty() {
            return Ok((0, 0));
        }

        let transcript = Transcript::new(
            stored
                .iter()
                .map(|s| Segment {
                    text: s.text.clone(),
                    start_ms: s.start_ms,
                    end_ms: s.end_ms,
                    confidence: s.confidence,
                    speaker: s.speaker.clone(),
                })
                .collect(),
        );

        let labelled = self.diarizer.diarize(&transcript)?;

        let mut speakers = std::collections::HashSet::new();
        let mut count = 0;

        // Zip by position: diarization preserves order and length, so index `i` of the
        // result corresponds to `stored[i]`.
        for (stored, labelled) in stored.iter().zip(labelled.segments.iter()) {
            if let Some(speaker) = &labelled.speaker {
                repo.set_segment_speaker(stored.id, speaker)?;
                speakers.insert(speaker.clone());
                count += 1;
            }
        }

        Ok((count, speakers.len()))
    }
}

/// Write segments, optionally forcing a speaker label.
///
/// `speaker` overrides whatever the engine reported. Channel recording uses it: the label is
/// known from the capture source before a word is decoded, so there is nothing to infer.
fn store_segments(
    db: &Database,
    meeting_id: Id,
    segments: &[Segment],
    speaker: Option<&str>,
) -> Result<usize> {
    if segments.is_empty() {
        return Ok(0);
    }

    let repo = MeetingRepository::new(db);
    let batch: Vec<NewTranscriptSegment> = segments
        .iter()
        .map(|s| NewTranscriptSegment {
            meeting_id,
            speaker: speaker.map(str::to_string).or_else(|| s.speaker.clone()),
            text: s.text.clone(),
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            confidence: s.confidence,
        })
        .collect();

    Ok(repo.add_segments(batch)?.len())
}

/// Which capture source audio arrived on.
///
/// The channel *is* the speaker, on a call. Everything the microphone hears is the person at
/// this machine; everything the system tap hears is the far end. That is not an inference from
/// timings or a clustering of voices — it is which cable the audio came down, and it is the
/// most reliable speaker signal available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// The local microphone: whoever is sitting at this machine.
    Microphone,
    /// The system audio tap: everyone on the far end of the call.
    System,
}

impl Channel {
    /// The label segments from this channel are attributed to.
    pub fn speaker_label(self) -> &'static str {
        match self {
            Channel::Microphone => "You",
            Channel::System => "Others",
        }
    }
}

/// One capture source and the engine transcribing it.
#[derive(Debug)]
pub struct ChannelInput {
    channel: Channel,
    source: Box<dyn AudioSource>,
    engine: Box<dyn TranscriptionEngine>,
    exhausted: bool,
    audio_ms: i64,
    segments: usize,
}

impl ChannelInput {
    pub fn new(
        channel: Channel,
        source: Box<dyn AudioSource>,
        engine: Box<dyn TranscriptionEngine>,
    ) -> Self {
        Self {
            channel,
            source,
            engine,
            exhausted: false,
            audio_ms: 0,
            segments: 0,
        }
    }

    pub fn channel(&self) -> Channel {
        self.channel
    }
}

/// Records several channels at once, attributing each segment to the channel it arrived on.
///
/// # Why this is not [`Pipeline`] with a mixer
///
/// [`Pipeline::mixed_source`] sums the microphone and the system tap into one mono stream, and
/// summing is irreversible: once the two are added together, no amount of downstream cleverness
/// recovers which side of the call a sentence came from. The pipeline then has to *infer* the
/// speaker from the mixed audio — from pauses, or from clustering voice embeddings — and both
/// are guesses that get it wrong.
///
/// Keeping the streams apart makes the question disappear for the case that matters most. On a
/// video call the microphone is you and the system tap is everyone else, so "who said this" is
/// answered by which stream it arrived on, exactly, for free, with no model.
///
/// Diarization does not run here, and that is deliberate: it would overwrite known attribution
/// with an inferred one. Separating several voices *within* one channel — three people around
/// one microphone — is a different problem, and the job of a [`Diarizer`] applied per channel.
///
/// # Cost
///
/// One engine per channel, but not one model per channel: engines that can share loaded weights
/// (see `WhisperEngine::sibling`) should be built that way by the caller. What is not shared is
/// the speech gating, which is per-channel by necessity — two streams cutting each other's
/// phrases would be worse than mixing them.
#[derive(Debug)]
pub struct ChannelPipeline {
    inputs: Vec<ChannelInput>,
}

impl ChannelPipeline {
    /// Build a pipeline over one or more channels.
    pub fn new(inputs: Vec<ChannelInput>) -> Result<Self> {
        if inputs.is_empty() {
            return Err(RecorderError::NoInput);
        }
        Ok(Self { inputs })
    }

    /// Run every channel to completion, storing segments as they are produced.
    pub async fn run(
        &mut self,
        db: &Database,
        meeting_id: Id,
        mut should_stop: impl FnMut() -> bool,
    ) -> Result<RecordingStats> {
        let mut stats = RecordingStats::default();

        loop {
            if should_stop() {
                tracing::debug!("stop requested");
                break;
            }

            // One frame from each live channel per turn. Channels are read in the same order
            // every time so a fast source cannot starve a slow one.
            let mut progressed = false;

            for input in &mut self.inputs {
                if input.exhausted {
                    continue;
                }

                let Some(frame) = input.source.next_frame()? else {
                    input.exhausted = true;
                    continue;
                };

                progressed = true;
                stats.frames_processed += 1;
                input.audio_ms += frame.duration_ms();

                let required = input.engine.required_format();
                let ready = if frame.format == required {
                    input.engine.feed(&frame).await?
                } else {
                    input.engine.feed(&frame.to_transcription_format()).await?
                };

                let stored =
                    store_segments(db, meeting_id, &ready, Some(input.channel.speaker_label()))?;
                input.segments += stored;
                stats.segments_stored += stored;
            }

            // Every channel is exhausted.
            if !progressed {
                break;
            }
        }

        for input in &mut self.inputs {
            let remaining = input.engine.finish().await?;
            let stored = store_segments(
                db,
                meeting_id,
                &remaining,
                Some(input.channel.speaker_label()),
            )?;
            input.segments += stored;
            stats.segments_stored += stored;
        }

        // Wall clock, so the longest channel — not the sum, which would report a two-channel
        // recording as twice its real length.
        stats.audio_ms = self.inputs.iter().map(|i| i.audio_ms).max().unwrap_or(0);
        stats.segments_attributed = stats.segments_stored;
        stats.speakers_detected = self.inputs.iter().filter(|i| i.segments > 0).count();

        tracing::info!(
            frames = stats.frames_processed,
            segments = stats.segments_stored,
            channels = self.inputs.len(),
            speakers = stats.speakers_detected,
            "channel recording finished"
        );

        Ok(stats)
    }
}

/// Record the relationship between a meeting and the transcript it produced.
///
/// Separate from the pipeline so importing an existing recording and capturing a live one
/// leave the same graph shape.
pub fn link_meeting_to_project(db: &Database, meeting_id: Id, project_id: Id) -> Result<()> {
    Graph::new(db).connect(
        NodeRef::new(NodeKind::Project, project_id),
        EdgeKind::Contains,
        NodeRef::new(NodeKind::Meeting, meeting_id),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use notewise_audio_capture::{
        AudioFormat, CaptureConfig, FileSource, SampleRate, SyntheticSource, Waveform,
    };
    use notewise_diarization::NoopDiarizer;
    use notewise_storage::{MeetingSource, NewMeeting};
    use notewise_transcription::MockEngine;

    fn db() -> Database {
        Database::open_in_memory().expect("in-memory db")
    }

    fn meeting(db: &Database) -> Id {
        MeetingRepository::new(db)
            .create(NewMeeting {
                project_id: None,
                title: "Pipeline test".into(),
                source: MeetingSource::Combined,
                started_at: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            })
            .expect("create meeting")
            .id
    }

    fn tone(duration_ms: u32) -> SyntheticSource {
        SyntheticSource::new(
            Waveform::Sine { hz: 440 },
            duration_ms,
            &CaptureConfig::default(),
        )
    }

    fn never_stop() -> impl FnMut() -> bool {
        || false
    }

    #[tokio::test]
    async fn audio_becomes_stored_transcript() {
        // The whole point: end to end, audio in, rows out.
        let db = db();
        let id = meeting(&db);
        let mut pipeline = Pipeline::new(Box::new(MockEngine::new()));

        let stats = pipeline
            .run(&db, id, &mut tone(1000), never_stop())
            .await
            .expect("pipeline");

        assert_eq!(stats.frames_processed, 10);
        assert!(stats.segments_stored > 0);
        assert_eq!(stats.audio_ms, 1000);

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        assert_eq!(stored.len(), stats.segments_stored);
    }

    #[tokio::test]
    async fn segments_are_stored_during_recording_not_only_at_the_end() {
        // A live transcript that only appears at the end is not a live transcript. Stopping
        // part-way must still leave rows written by the loop, not just by the final flush.
        let db = db();
        let id = meeting(&db);
        let mut pipeline = Pipeline::new(Box::new(MockEngine::new())).with_config(PipelineConfig {
            diarize: false,
            ..Default::default()
        });

        let mut frames = 0;
        pipeline
            .run(&db, id, &mut tone(60_000), move || {
                frames += 1;
                frames > 6
            })
            .await
            .expect("pipeline");

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        assert!(
            stored.len() >= 4,
            "expected segments written during the loop, got {}",
            stored.len()
        );
    }

    #[tokio::test]
    async fn stopping_ends_a_source_that_would_never_exhaust() {
        // A microphone never returns None; without this a recording could not be stopped.
        let db = db();
        let id = meeting(&db);
        let mut pipeline = Pipeline::new(Box::new(MockEngine::new()));

        let mut frames = 0;
        let stats = pipeline
            .run(&db, id, &mut tone(60_000), move || {
                frames += 1;
                frames > 5
            })
            .await
            .expect("pipeline");

        assert!(
            stats.frames_processed <= 6,
            "should have stopped early, processed {}",
            stats.frames_processed
        );
        assert!(stats.audio_ms < 60_000);
    }

    #[tokio::test]
    async fn buffered_audio_is_flushed_when_stopping() {
        // Whatever the engine is holding must not be lost — that is the last thing anyone
        // said before the meeting ended.
        let db = db();
        let id = meeting(&db);
        let mut pipeline = Pipeline::new(Box::new(MockEngine::new()));

        // 300ms = 3 frames; MockEngine emits in pairs, so one is left buffered.
        pipeline
            .run(&db, id, &mut tone(300), never_stop())
            .await
            .expect("pipeline");

        assert_eq!(
            MeetingRepository::new(&db).segments(id).unwrap().len(),
            3,
            "the odd buffered segment should have been flushed"
        );
    }

    #[tokio::test]
    async fn speakers_are_assigned_by_the_diarization_pass() {
        let db = db();
        let id = meeting(&db);
        let mut pipeline = Pipeline::new(Box::new(MockEngine::new()));

        let stats = pipeline
            .run(&db, id, &mut tone(1000), never_stop())
            .await
            .expect("pipeline");

        assert!(stats.segments_attributed > 0);
        assert!(stats.speakers_detected >= 1);

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        assert!(
            stored.iter().all(|s| s.speaker.is_some()),
            "every segment should be labelled after the final pass"
        );
    }

    #[tokio::test]
    async fn diarization_can_be_turned_off() {
        let db = db();
        let id = meeting(&db);
        let mut pipeline = Pipeline::new(Box::new(MockEngine::new())).with_config(PipelineConfig {
            diarize: false,
            ..Default::default()
        });

        let stats = pipeline
            .run(&db, id, &mut tone(1000), never_stop())
            .await
            .expect("pipeline");

        assert_eq!(stats.segments_attributed, 0);
        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        assert!(stored.iter().all(|s| s.speaker.is_none()));
    }

    #[tokio::test]
    async fn a_noop_diarizer_leaves_segments_unattributed() {
        let db = db();
        let id = meeting(&db);
        let mut pipeline =
            Pipeline::new(Box::new(MockEngine::new())).with_diarizer(Box::new(NoopDiarizer));

        let stats = pipeline
            .run(&db, id, &mut tone(1000), never_stop())
            .await
            .expect("pipeline");

        assert_eq!(stats.segments_attributed, 0);
    }

    #[tokio::test]
    async fn silence_records_no_segments_but_still_completes() {
        // A meeting nobody spoke in is a valid meeting, not an error.
        let db = db();
        let id = meeting(&db);
        let mut pipeline = Pipeline::new(Box::new(MockEngine::new()));

        let stats = pipeline
            .run(&db, id, &mut SyntheticSource::silence(), never_stop())
            .await
            .expect("pipeline");

        assert_eq!(stats.frames_processed, 10);
        assert_eq!(stats.segments_stored, 0);
        assert_eq!(stats.speakers_detected, 0);
    }

    #[tokio::test]
    async fn a_source_in_the_wrong_format_is_converted_not_rejected() {
        // What a real system tap produces: 48kHz stereo.
        let db = db();
        let id = meeting(&db);
        let mut source = FileSource::from_samples(
            vec![0.5; 96_000],
            AudioFormat::new(SampleRate::STUDIO, 2),
            100,
        );

        let mut pipeline = Pipeline::new(Box::new(MockEngine::new()));
        let stats = pipeline
            .run(&db, id, &mut source, never_stop())
            .await
            .expect("should convert, not reject");

        assert!(stats.segments_stored > 0);
    }

    #[tokio::test]
    async fn timestamps_are_chronological_and_non_overlapping() {
        let db = db();
        let id = meeting(&db);
        let mut pipeline = Pipeline::new(Box::new(MockEngine::new()));
        pipeline
            .run(&db, id, &mut tone(2000), never_stop())
            .await
            .unwrap();

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        for pair in stored.windows(2) {
            assert!(pair[0].start_ms <= pair[1].start_ms);
            assert!(
                pair[0].end_ms <= pair[1].start_ms,
                "{:?} overlaps {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[tokio::test]
    async fn the_transcript_reads_back_as_text() {
        let db = db();
        let id = meeting(&db);
        Pipeline::new(Box::new(MockEngine::new()))
            .run(&db, id, &mut tone(1000), never_stop())
            .await
            .unwrap();

        let text = MeetingRepository::new(&db).transcript_text(id).unwrap();
        assert!(text.contains("Speaker 1"), "{text}");
        assert!(text.contains("Mock segment"), "{text}");
    }

    #[test]
    fn a_mixed_source_needs_at_least_one_input() {
        assert!(matches!(
            Pipeline::mixed_source(None, None).unwrap_err(),
            RecorderError::NoInput
        ));

        assert!(Pipeline::mixed_source(Some(Box::new(SyntheticSource::silence())), None).is_ok());
    }

    #[tokio::test]
    async fn a_mixed_source_records_through_the_pipeline() {
        // Both sides of a meeting, summed and transcribed as one stream.
        let db = db();
        let id = meeting(&db);

        let mut source = Pipeline::mixed_source(
            Some(Box::new(tone(1000))),
            Some(Box::new(SyntheticSource::new(
                Waveform::Sine { hz: 880 },
                1000,
                &CaptureConfig::default(),
            ))),
        )
        .expect("mixed source");

        let stats = Pipeline::new(Box::new(MockEngine::new()))
            .run(&db, id, &mut source, never_stop())
            .await
            .expect("pipeline");

        assert_eq!(stats.frames_processed, 10);
        assert!(stats.segments_stored > 0);
    }

    /// The whole pipeline with real inference: WAV -> Whisper -> diarize -> SQLite.
    ///
    /// Ignored because it needs a downloaded model and a speech sample. Run with:
    /// `NOTEWISE_MODEL_DIR=... NOTEWISE_SAMPLE_WAV=... cargo test -p notewise-recorder \
    ///   --features whisper-metal -- --ignored --nocapture`
    #[tokio::test]
    #[cfg(feature = "whisper")]
    #[ignore = "requires a downloaded model and a speech sample"]
    async fn records_real_speech_end_to_end() {
        use notewise_transcription::{ModelRegistry, ModelStore, WhisperEngine};

        let model_dir = std::env::var("NOTEWISE_MODEL_DIR").expect("NOTEWISE_MODEL_DIR");
        let sample = std::env::var("NOTEWISE_SAMPLE_WAV").expect("NOTEWISE_SAMPLE_WAV");

        let db = db();
        let id = meeting(&db);

        let engine =
            WhisperEngine::new(ModelRegistry::default_model(), ModelStore::new(&model_dir))
                .expect("whisper engine");

        let mut source = FileSource::open_wav(&sample).expect("wav");
        let mut pipeline = Pipeline::new(Box::new(engine));

        let started = std::time::Instant::now();
        let stats = pipeline
            .run(&db, id, &mut source, never_stop())
            .await
            .expect("pipeline");
        let elapsed = started.elapsed();

        let repo = MeetingRepository::new(&db);
        let text = repo.transcript_text(id).unwrap();

        println!("\n--- stored transcript ---\n{text}");
        println!(
            "{} frames, {} segments, {} speakers, {:.2}s of audio in {:.2}s ({:.1}x realtime)",
            stats.frames_processed,
            stats.segments_stored,
            stats.speakers_detected,
            stats.audio_ms as f64 / 1000.0,
            elapsed.as_secs_f64(),
            (stats.audio_ms as f64 / 1000.0) / elapsed.as_secs_f64(),
        );

        // Real words, actually persisted.
        let lower = text.to_lowercase();
        assert!(lower.contains("postgres"), "{text}");
        assert!(lower.contains("friday"), "{text}");

        // And the rows are real rows, with speakers and sane timings.
        let stored = repo.segments(id).unwrap();
        assert!(!stored.is_empty());
        assert!(
            stored.iter().all(|s| s.speaker.is_some()),
            "diarization did not run"
        );
        assert!(stored.iter().all(|s| s.end_ms > s.start_ms));
    }

    /// The regression test for the phantom speaker.
    ///
    /// These are the exact rows a real 15-second recording left in the database: one person
    /// said "hello how are you", stopped, and the transcript came back attributed to two
    /// people. The eight-second hole is what the old fixed-window engine left behind, and the
    /// pause heuristic read it as a change of speaker.
    ///
    /// Fixing the windowing removes the hole, but this asserts the other half: the pipeline's
    /// default must not invent a second person out of silence even when a hole is there. A gap
    /// is not evidence about who was talking.
    #[tokio::test]
    async fn a_gap_in_the_transcript_is_not_a_second_speaker() {
        /// Emits a fixed script of segments, so the shape of a real recording can be replayed
        /// without a model.
        #[derive(Debug)]
        struct ScriptedEngine(Vec<Segment>);

        #[notewise_transcription::async_trait]
        impl TranscriptionEngine for ScriptedEngine {
            fn name(&self) -> &str {
                "scripted"
            }
            async fn feed(
                &mut self,
                _frame: &notewise_audio_capture::AudioFrame,
            ) -> notewise_transcription::Result<Vec<Segment>> {
                Ok(Vec::new())
            }
            async fn finish(&mut self) -> notewise_transcription::Result<Vec<Segment>> {
                Ok(std::mem::take(&mut self.0))
            }
        }

        let db = db();
        let id = meeting(&db);

        let engine = ScriptedEngine(vec![
            Segment::new("hello how are you", 0, 2_000),
            Segment::new("Okay.", 10_000, 11_000),
            Segment::new("Okay.", 11_000, 12_000),
            Segment::new("Okay.", 12_000, 13_000),
            Segment::new("Okay.", 13_000, 14_000),
        ]);

        let stats = Pipeline::new(Box::new(engine))
            .run(&db, id, &mut tone(300), never_stop())
            .await
            .expect("pipeline");

        assert_eq!(
            stats.speakers_detected, 1,
            "one person in the room became {} speakers",
            stats.speakers_detected
        );

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        let speakers: std::collections::HashSet<_> =
            stored.iter().filter_map(|s| s.speaker.clone()).collect();
        assert_eq!(speakers.len(), 1, "labelled {speakers:?}");
        assert!(
            stored.iter().all(|s| s.speaker.is_some()),
            "segments should still be attributed, just not to invented people"
        );
    }

    /// The point of channel recording: attribution is exact, not inferred.
    ///
    /// Two people talking over each other is the case every timing heuristic and every
    /// embedding clusterer gets wrong. Arriving on two separate streams, it is not a hard case
    /// at all — each word is labelled by the cable it came down.
    #[tokio::test]
    async fn each_channel_is_attributed_to_its_own_speaker() {
        let db = db();
        let id = meeting(&db);

        let mut pipeline = ChannelPipeline::new(vec![
            ChannelInput::new(
                Channel::Microphone,
                Box::new(tone(1_000)),
                Box::new(MockEngine::new()),
            ),
            ChannelInput::new(
                Channel::System,
                Box::new(tone(1_000)),
                Box::new(MockEngine::new()),
            ),
        ])
        .expect("two channels");

        let stats = pipeline.run(&db, id, never_stop()).await.expect("pipeline");

        assert_eq!(stats.speakers_detected, 2, "one speaker per channel");
        assert!(stats.segments_stored > 0);

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        let speakers: std::collections::HashSet<_> =
            stored.iter().filter_map(|s| s.speaker.clone()).collect();
        assert_eq!(
            speakers,
            ["You".to_string(), "Others".to_string()]
                .into_iter()
                .collect(),
            "got {speakers:?}"
        );
        assert!(
            stored.iter().all(|s| s.speaker.is_some()),
            "channel recording knows every speaker before it decodes a word"
        );
    }

    /// A two-channel recording is as long as the meeting, not twice as long.
    #[tokio::test]
    async fn audio_length_is_wall_clock_not_the_sum_of_channels() {
        let db = db();
        let id = meeting(&db);

        let mut pipeline = ChannelPipeline::new(vec![
            ChannelInput::new(
                Channel::Microphone,
                Box::new(tone(1_000)),
                Box::new(MockEngine::new()),
            ),
            ChannelInput::new(
                Channel::System,
                Box::new(tone(1_000)),
                Box::new(MockEngine::new()),
            ),
        ])
        .unwrap();

        let stats = pipeline.run(&db, id, never_stop()).await.unwrap();
        assert_eq!(
            stats.audio_ms, 1_000,
            "summed the channels instead of taking the span"
        );
    }

    /// One side of a call going quiet must not end the recording.
    #[tokio::test]
    async fn one_channel_ending_early_does_not_stop_the_other() {
        let db = db();
        let id = meeting(&db);

        let mut pipeline = ChannelPipeline::new(vec![
            ChannelInput::new(
                Channel::Microphone,
                Box::new(tone(300)),
                Box::new(MockEngine::new()),
            ),
            ChannelInput::new(
                Channel::System,
                Box::new(tone(2_000)),
                Box::new(MockEngine::new()),
            ),
        ])
        .unwrap();

        let stats = pipeline.run(&db, id, never_stop()).await.unwrap();

        assert_eq!(
            stats.audio_ms, 2_000,
            "the longer channel should have run to its end"
        );
        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        assert!(
            stored
                .iter()
                .any(|s| s.speaker.as_deref() == Some("Others")),
            "the channel that kept going produced nothing"
        );
    }

    /// Recording with only a microphone is the common case and must still work.
    #[tokio::test]
    async fn a_single_channel_recording_is_valid() {
        let db = db();
        let id = meeting(&db);

        let mut pipeline = ChannelPipeline::new(vec![ChannelInput::new(
            Channel::Microphone,
            Box::new(tone(1_000)),
            Box::new(MockEngine::new()),
        )])
        .unwrap();

        let stats = pipeline.run(&db, id, never_stop()).await.unwrap();
        assert_eq!(stats.speakers_detected, 1);

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        assert!(stored.iter().all(|s| s.speaker.as_deref() == Some("You")));
    }

    #[test]
    fn a_channel_pipeline_needs_a_channel() {
        assert!(matches!(
            ChannelPipeline::new(Vec::new()).unwrap_err(),
            RecorderError::NoInput
        ));
    }

    /// Stopping must end a channel recording promptly, as it does a mixed one.
    #[tokio::test]
    async fn stopping_ends_a_channel_recording() {
        let db = db();
        let id = meeting(&db);

        let mut pipeline = ChannelPipeline::new(vec![ChannelInput::new(
            Channel::Microphone,
            Box::new(tone(60_000)),
            Box::new(MockEngine::new()),
        )])
        .unwrap();

        let mut turns = 0;
        let stats = pipeline
            .run(&db, id, move || {
                turns += 1;
                turns > 5
            })
            .await
            .unwrap();

        assert!(stats.audio_ms < 60_000, "ran to {} ms", stats.audio_ms);
    }

    #[tokio::test]
    async fn an_engine_failure_surfaces_rather_than_recording_silence() {
        // A broken engine must not look like a meeting where nobody spoke.
        #[derive(Debug)]
        struct BrokenEngine;

        #[notewise_transcription::async_trait]
        impl TranscriptionEngine for BrokenEngine {
            fn name(&self) -> &str {
                "broken"
            }
            async fn feed(
                &mut self,
                _frame: &notewise_audio_capture::AudioFrame,
            ) -> notewise_transcription::Result<Vec<Segment>> {
                Err(TranscriptionError::BadAudio("simulated failure".into()))
            }
            async fn finish(&mut self) -> notewise_transcription::Result<Vec<Segment>> {
                Ok(Vec::new())
            }
        }

        let db = db();
        let id = meeting(&db);

        let err = Pipeline::new(Box::new(BrokenEngine))
            .run(&db, id, &mut tone(500), never_stop())
            .await
            .expect_err("should surface the failure");

        assert!(matches!(err, RecorderError::Transcription(_)));
    }
}
