//! The recording pipeline.
//!
//! Connects the pieces that until now existed only as separate, individually-tested
//! components: **capture → mix → transcribe → diarize → store**.
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
use notewise_diarization::{DiarizationError, Diarizer, PauseDiarizer};
use notewise_graph::{EdgeKind, Graph, GraphError, NodeKind, NodeRef};
use notewise_storage::{
    Database, Id, MeetingRepository, NewTranscriptSegment, StorageError,
};
use notewise_transcription::{
    Segment, Transcript, TranscriptionEngine, TranscriptionError,
};

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
    pub fn new(engine: Box<dyn TranscriptionEngine>) -> Self {
        Self {
            engine,
            diarizer: Box::new(PauseDiarizer::default()),
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
        if segments.is_empty() {
            return Ok(0);
        }

        let repo = MeetingRepository::new(db);
        let batch: Vec<NewTranscriptSegment> = segments
            .iter()
            .map(|s| NewTranscriptSegment {
                meeting_id,
                // Left unattributed on purpose; filled in by the diarization pass.
                speaker: s.speaker.clone(),
                text: s.text.clone(),
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                confidence: s.confidence,
            })
            .collect();

        Ok(repo.add_segments(batch)?.len())
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
        let mut pipeline = Pipeline::new(Box::new(MockEngine::new()))
            .with_config(PipelineConfig { diarize: false, ..Default::default() });

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

        let engine = WhisperEngine::new(
            ModelRegistry::default_model(),
            ModelStore::new(&model_dir),
        )
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
        assert!(stored.iter().all(|s| s.speaker.is_some()), "diarization did not run");
        assert!(stored.iter().all(|s| s.end_ms > s.start_ms));
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
