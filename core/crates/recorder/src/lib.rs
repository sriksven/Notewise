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

    /// Two inputs claimed the same capture channel.
    ///
    /// Channels are identified downstream by the speaker label they write, so two inputs on one
    /// channel would be indistinguishable in storage — and a per-channel refinement pass would
    /// silently re-label both.
    #[error("channel {channel:?} was attached twice")]
    DuplicateChannel { channel: Channel },
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
    /// Splits this one channel into several speakers once recording stops. See
    /// [`ChannelInput::with_diarizer`].
    diarizer: Option<Box<dyn Diarizer + Send>>,
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
            diarizer: None,
            exhausted: false,
            audio_ms: 0,
            segments: 0,
        }
    }

    /// Split this channel's segments into individual speakers after recording.
    ///
    /// # What this is for
    ///
    /// The channel label answers "which side of the call" and stops there. On a five-person
    /// video call the system tap is one channel carrying four people, all stored as `Others` —
    /// correct, and not what a reader wants. This is the hook that turns those four into four
    /// names.
    ///
    /// The intended argument is [`notewise_diarization::TimelineDiarizer`], carrying speaker
    /// events reported by the meeting platform. That needs no audio and no model: the platform
    /// routing the call already knows who was unmuted, so this is an interval join rather than an
    /// inference.
    ///
    /// # Why it runs after, not during
    ///
    /// The same reason the rest of the pipeline works this way — the label is refined once the
    /// whole channel is known, while the live transcript still appears immediately under its
    /// channel label. A reader sees `Others` during the call and names afterwards, rather than
    /// nothing during the call.
    ///
    /// # A segment this leaves unlabelled keeps its channel label
    ///
    /// [`notewise_diarization::TimelineDiarizer`] returns `None` for a segment the platform
    /// reported no speaker for, rather than guessing a name. Those segments stay `Others`, which
    /// is still true. Refinement only ever replaces a coarse label with a specific one.
    pub fn with_diarizer(mut self, diarizer: Box<dyn Diarizer + Send>) -> Self {
        self.diarizer = Some(diarizer);
        self
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
/// Diarization does not run *across* channels, and that is deliberate: it would overwrite known
/// attribution with an inferred one.
///
/// Separating several voices *within* one channel is a different problem, and the common one — a
/// five-person call puts four people on the system tap, and three people around one microphone
/// are one channel too. [`ChannelInput::with_diarizer`] is that hook: a per-channel pass that
/// refines `Others` into names without touching what the channel already established.
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
    ///
    /// # Errors
    ///
    /// [`RecorderError::DuplicateChannel`] if two inputs share a channel. Stored segments carry
    /// only the channel's speaker label, so two inputs on one channel cannot be told apart
    /// afterwards — and [`ChannelInput::with_diarizer`] would then re-label both from one
    /// channel's evidence.
    pub fn new(inputs: Vec<ChannelInput>) -> Result<Self> {
        if inputs.is_empty() {
            return Err(RecorderError::NoInput);
        }

        for (index, input) in inputs.iter().enumerate() {
            if inputs[..index].iter().any(|e| e.channel == input.channel) {
                return Err(RecorderError::DuplicateChannel {
                    channel: input.channel,
                });
            }
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

        // Split any channel that carries more than one person. Runs after every channel has
        // finished, so a diarizer sees that channel's whole transcript.
        let refined = self.refine(db, meeting_id)?;
        if refined > 0 {
            stats.speakers_detected = distinct_speakers(db, meeting_id)?;
        }

        tracing::info!(
            frames = stats.frames_processed,
            segments = stats.segments_stored,
            channels = self.inputs.len(),
            refined,
            speakers = stats.speakers_detected,
            "channel recording finished"
        );

        Ok(stats)
    }

    /// Run each channel's diarizer over that channel's stored segments.
    ///
    /// Returns how many segments were given a more specific speaker than their channel label.
    fn refine(&self, db: &Database, meeting_id: Id) -> Result<usize> {
        let mut refined = 0;

        for input in &self.inputs {
            let Some(diarizer) = &input.diarizer else {
                continue;
            };
            refined += refine_channel(db, meeting_id, input.channel, diarizer.as_ref())?;
        }

        Ok(refined)
    }
}

/// Re-label one channel's stored segments using a per-channel diarizer.
///
/// # Identifying a channel's segments
///
/// By the speaker label the channel wrote. That is the only marker storage keeps, and it is
/// sufficient because [`ChannelPipeline::new`] rejects two inputs on one channel — so a label maps
/// to exactly one channel for the life of a recording.
///
/// # What is not overwritten
///
/// A segment the diarizer returns unlabelled keeps its channel label. `Others` is a true
/// statement about a segment the platform reported no speaker for; replacing it with a guess, or
/// with nothing, would both be worse.
fn refine_channel(
    db: &Database,
    meeting_id: Id,
    channel: Channel,
    diarizer: &dyn Diarizer,
) -> Result<usize> {
    let repo = MeetingRepository::new(db);
    let label = channel.speaker_label();

    let mine: Vec<_> = repo
        .segments(meeting_id)?
        .into_iter()
        .filter(|s| s.speaker.as_deref() == Some(label))
        .collect();

    if mine.is_empty() {
        return Ok(0);
    }

    let transcript = Transcript::new(
        mine.iter()
            .map(|s| Segment {
                text: s.text.clone(),
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                confidence: s.confidence,
                speaker: s.speaker.clone(),
            })
            .collect(),
    );

    let labelled = diarizer.diarize(&transcript)?;
    let mut count = 0;

    // Zip by position: diarization preserves order and length.
    for (stored, labelled) in mine.iter().zip(labelled.segments.iter()) {
        match &labelled.speaker {
            // Unchanged, or declined. Either way there is nothing more specific to write.
            None => continue,
            Some(speaker) if speaker == label => continue,
            Some(speaker) => {
                repo.set_segment_speaker(stored.id, speaker)?;
                count += 1;
            }
        }
    }

    tracing::debug!(
        ?channel,
        diarizer = diarizer.name(),
        segments = mine.len(),
        refined = count,
        "refined a channel's speakers"
    );

    Ok(count)
}

/// How many distinct speakers a meeting's stored segments name.
fn distinct_speakers(db: &Database, meeting_id: Id) -> Result<usize> {
    let speakers: std::collections::HashSet<String> = MeetingRepository::new(db)
        .segments(meeting_id)?
        .into_iter()
        .filter_map(|s| s.speaker)
        .collect();

    Ok(speakers.len())
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

    /// An engine that emits a fixed script, so a test controls segment timings exactly.
    ///
    /// Timings are what a timeline joins against, so they cannot be left to a mock's discretion.
    #[derive(Debug)]
    struct PlannedEngine(Vec<Segment>);

    #[notewise_transcription::async_trait]
    impl TranscriptionEngine for PlannedEngine {
        fn name(&self) -> &str {
            "planned"
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

    /// Four participants, each holding the floor for five seconds.
    fn four_people() -> notewise_diarization::SpeakerTimeline {
        use notewise_diarization::{Participant, ParticipantId, SpeakerTimeline};

        let mut timeline = SpeakerTimeline::new();
        for (i, name) in ["Priya", "Marcus", "Ana", "Jun"].iter().enumerate() {
            timeline.upsert_participant(Participant::new(format!("p{i}"), *name));
            timeline
                .add_turn(
                    ParticipantId::new(format!("p{i}")),
                    i as i64 * 5_000,
                    i as i64 * 5_000 + 5_000,
                )
                .expect("turn");
        }
        timeline
    }

    /// The requirement that motivated all of this: five people on a call, each voice named.
    ///
    /// The microphone is the user. The system tap carries the other four, which the channel label
    /// can only call `Others` — so the platform timeline splits that one channel into four names.
    #[tokio::test]
    async fn a_channel_carrying_several_people_is_split_into_names() {
        use notewise_diarization::TimelineDiarizer;

        let db = db();
        let id = meeting(&db);

        let remote = PlannedEngine(vec![
            Segment::new("first point", 0, 4_000),
            Segment::new("second point", 5_000, 9_000),
            Segment::new("third point", 10_000, 14_000),
            Segment::new("fourth point", 15_000, 19_000),
        ]);

        let mut pipeline = ChannelPipeline::new(vec![
            ChannelInput::new(
                Channel::Microphone,
                Box::new(tone(500)),
                Box::new(MockEngine::new()),
            ),
            ChannelInput::new(Channel::System, Box::new(tone(500)), Box::new(remote))
                .with_diarizer(Box::new(TimelineDiarizer::new(four_people()))),
        ])
        .expect("two channels");

        let stats = pipeline.run(&db, id, never_stop()).await.expect("pipeline");

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        let speakers: std::collections::HashSet<_> =
            stored.iter().filter_map(|s| s.speaker.clone()).collect();

        assert!(
            !speakers.contains("Others"),
            "every remote segment should have been named, got {speakers:?}"
        );
        for name in ["Priya", "Marcus", "Ana", "Jun"] {
            assert!(speakers.contains(name), "missing {name} in {speakers:?}");
        }
        assert!(
            speakers.contains("You"),
            "the microphone channel keeps its own label, got {speakers:?}"
        );
        assert_eq!(
            stats.speakers_detected, 5,
            "four named remotes plus the local user, got {speakers:?}"
        );
    }

    /// A gap in the platform feed must not become a name.
    ///
    /// The channel label is still true for a segment nobody was reported speaking over, and a
    /// borrowed name from a neighbouring turn would put words in a real colleague's mouth.
    #[tokio::test]
    async fn a_segment_the_platform_never_reported_keeps_its_channel_label() {
        use notewise_diarization::{Participant, ParticipantId, SpeakerTimeline, TimelineDiarizer};

        let db = db();
        let id = meeting(&db);

        let mut timeline = SpeakerTimeline::new();
        timeline.upsert_participant(Participant::new("p1", "Priya"));
        timeline
            .add_turn(ParticipantId::new("p1"), 0, 5_000)
            .expect("turn");

        let remote = PlannedEngine(vec![
            Segment::new("covered", 0, 4_000),
            // Long after the feed stopped reporting.
            Segment::new("uncovered", 60_000, 64_000),
        ]);

        let mut pipeline = ChannelPipeline::new(vec![ChannelInput::new(
            Channel::System,
            Box::new(tone(500)),
            Box::new(remote),
        )
        .with_diarizer(Box::new(TimelineDiarizer::new(timeline)))])
        .expect("one channel");

        pipeline.run(&db, id, never_stop()).await.expect("pipeline");

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        let by_text: std::collections::HashMap<_, _> = stored
            .iter()
            .map(|s| (s.text.as_str(), s.speaker.as_deref()))
            .collect();

        assert_eq!(by_text.get("covered"), Some(&Some("Priya")));
        assert_eq!(
            by_text.get("uncovered"),
            Some(&Some("Others")),
            "an unreported segment keeps a true coarse label rather than gaining a false name"
        );
    }

    /// Refinement is scoped to the channel that asked for it.
    #[tokio::test]
    async fn one_channels_diarizer_does_not_relabel_another_channel() {
        use notewise_diarization::TimelineDiarizer;

        let db = db();
        let id = meeting(&db);

        // The microphone's segments sit inside the timeline's turns, so a diarizer applied to the
        // wrong channel would rename them.
        let local = PlannedEngine(vec![Segment::new("my words", 0, 4_000)]);
        let remote = PlannedEngine(vec![Segment::new("their words", 5_000, 9_000)]);

        let mut pipeline = ChannelPipeline::new(vec![
            ChannelInput::new(Channel::Microphone, Box::new(tone(500)), Box::new(local)),
            ChannelInput::new(Channel::System, Box::new(tone(500)), Box::new(remote))
                .with_diarizer(Box::new(TimelineDiarizer::new(four_people()))),
        ])
        .expect("two channels");

        pipeline.run(&db, id, never_stop()).await.expect("pipeline");

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        let by_text: std::collections::HashMap<_, _> = stored
            .iter()
            .map(|s| (s.text.as_str(), s.speaker.as_deref()))
            .collect();

        assert_eq!(
            by_text.get("my words"),
            Some(&Some("You")),
            "the microphone channel was not asked to be split"
        );
        assert_eq!(by_text.get("their words"), Some(&Some("Marcus")));
    }

    /// Without this, two inputs on one channel would both be refined from one channel's evidence.
    #[test]
    fn two_inputs_on_one_channel_are_rejected() {
        let error = ChannelPipeline::new(vec![
            ChannelInput::new(
                Channel::System,
                Box::new(tone(100)),
                Box::new(MockEngine::new()),
            ),
            ChannelInput::new(
                Channel::System,
                Box::new(tone(100)),
                Box::new(MockEngine::new()),
            ),
        ])
        .unwrap_err();

        assert!(
            matches!(
                error,
                RecorderError::DuplicateChannel {
                    channel: Channel::System
                }
            ),
            "got {error:?}"
        );
    }

    /// A channel with no diarizer behaves exactly as before.
    #[tokio::test]
    async fn a_channel_without_a_diarizer_is_unchanged() {
        let db = db();
        let id = meeting(&db);

        let mut pipeline = ChannelPipeline::new(vec![ChannelInput::new(
            Channel::System,
            Box::new(tone(1_000)),
            Box::new(MockEngine::new()),
        )])
        .expect("one channel");

        pipeline.run(&db, id, never_stop()).await.expect("pipeline");

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        assert!(
            stored
                .iter()
                .all(|s| s.speaker.as_deref() == Some("Others")),
            "refinement must be opt-in"
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
