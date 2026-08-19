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

use notewise_audio_capture::{
    AudioFormat, AudioSource, CaptureError, MixedSource, Mixer, WavWriter,
};
use notewise_diarization::{AudioDiarizer, DiarizationError, Diarizer, SingleSpeakerDiarizer};
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

/// The rate retained audio is held at, and the rate the acoustic diarizer is told.
///
/// Frames are converted to the transcription format before being retained, so retention has one
/// rate regardless of what the capture device produced — which is what lets a span's millisecond
/// bounds be turned back into sample offsets.
const RETENTION_SAMPLE_RATE: u32 = AudioFormat::transcription().sample_rate.hz();

/// What a recording produced.
// No longer `Copy`: it carries the retained audio's path, which is owned. Every caller
// clones or moves it, and none relied on implicit copies.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordingStats {
    pub frames_processed: usize,
    pub segments_stored: usize,
    /// Segments that received a speaker label during the final pass.
    pub segments_attributed: usize,
    pub speakers_detected: usize,
    pub audio_ms: i64,
    /// Where the audio was kept and how large the file is, when retention was on.
    pub retained_audio: Option<(std::path::PathBuf, u64)>,
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

/// Keep samples for an acoustic pass, within a millisecond budget.
///
/// Returns `false` once the budget would be exceeded, having **emptied** `audio`: a prefix cannot
/// be diarized safely — every segment past the cutoff would be labelled by `nearest_cluster` from
/// audio nobody examined — so holding it only wastes the memory the budget exists to bound.
///
/// One implementation for both [`Pipeline`] and [`ChannelInput`]. Two would be two places for the
/// off-by-one in the budget to differ.
fn retain_within(audio: &mut Vec<f32>, samples: &[f32], retain_ms: i64) -> bool {
    let budget = (RETENTION_SAMPLE_RATE as i64 * retain_ms / 1000).max(0) as usize;

    if audio.len() + samples.len() > budget {
        audio.clear();
        audio.shrink_to_fit();
        return false;
    }

    audio.extend_from_slice(samples);
    true
}

/// Runs one recording from audio to stored transcript.
#[derive(Debug)]
pub struct Pipeline {
    engine: Box<dyn TranscriptionEngine>,
    diarizer: ChannelDiarizer,
    /// Retained only for [`ChannelDiarizer::Audio`]. See [`Pipeline::with_audio_diarizer`].
    audio: Vec<f32>,
    /// Set when retention hit its budget: the buffer was dropped and the acoustic pass is skipped.
    audio_truncated: bool,
    /// Writes captured audio to disk, when the user asked for it to be kept.
    ///
    /// Separate from `audio` above, which is a bounded in-memory buffer serving acoustic speaker
    /// separation and is dropped when it hits its budget. This one is unbounded and durable, and its
    /// budget is the retention policy rather than a byte count.
    retain: Option<WavWriter>,
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
            diarizer: ChannelDiarizer::Transcript(Box::new(SingleSpeakerDiarizer)),
            audio: Vec::new(),
            retain: None,
            audio_truncated: false,
            config: PipelineConfig::default(),
        }
    }

    /// Keep the captured audio, writing it to `path` as it arrives.
    ///
    /// # Why the *converted* samples are what get written
    ///
    /// Each frame is written after `to_transcription_format`, at one known sample rate — the same
    /// conversion the acoustic retention buffer uses, and for the same reason. A transcript segment
    /// records millisecond bounds, and seeking to one means turning milliseconds into a sample
    /// offset. That arithmetic only works if the file is at a rate the reader can assume, which the
    /// raw device format is not.
    ///
    /// The writer is consumed by the run that uses it, so a pipeline reused for a second meeting
    /// retains nothing rather than appending to the first meeting's file.
    pub fn retaining_audio(mut self, path: impl AsRef<std::path::Path>) -> Result<Self> {
        self.retain = Some(WavWriter::create(path, AudioFormat::transcription())?);
        Ok(self)
    }

    pub fn with_diarizer(mut self, diarizer: Box<dyn Diarizer + Send>) -> Self {
        self.diarizer = ChannelDiarizer::Transcript(diarizer);
        self
    }

    /// Separate speakers from the audio, retaining at most `retain_ms` of it.
    ///
    /// This is how [`notewise_diarization::EmbeddingDiarizer`] reaches a **mono** recording — an
    /// imported file, or a single-microphone capture of a room. It is the case the channel path
    /// cannot help with: there is one stream and no platform timeline, so who spoke is only
    /// recoverable from the voices.
    ///
    /// Replaces any diarizer set by [`Self::with_diarizer`]; they are one field because asking
    /// for both is asking for the transcript pass to overwrite the acoustic one.
    ///
    /// # Why the budget is a required argument
    ///
    /// Clustering cannot begin until the whole recording is known, so the audio has to be kept.
    /// Mono 16 kHz `f32` is 64 KB per second — about 230 MB for an hour. That is not a cost to
    /// inherit from a default, so there is no default.
    ///
    /// # At the budget, it stops rather than guesses
    ///
    /// Retention stops, the buffer is dropped, and the acoustic pass is skipped with a warning;
    /// segments keep whatever the engine reported. Diarizing a prefix would label every later
    /// segment from audio that was never examined.
    pub fn with_audio_diarizer(
        mut self,
        diarizer: Box<dyn AudioDiarizer + Send>,
        retain_ms: i64,
    ) -> Self {
        self.diarizer = ChannelDiarizer::Audio {
            diarizer,
            retain_ms,
        };
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

            // Keep it on disk if the user asked for that. Before the diarizer's own buffer, so a
            // recording is retained in full even when acoustic separation gives up on its budget.
            if let Some(writer) = self.retain.as_mut() {
                writer.write_frame(&frame.to_transcription_format().samples)?;
            }

            // Convert once, here, so no engine has to care what the OS handed us.
            // Retain before feeding, and always from the transcription format, so a span's
            // millisecond bounds map to sample offsets at one known rate.
            if let ChannelDiarizer::Audio { retain_ms, .. } = self.diarizer {
                if !self.audio_truncated {
                    let converted = frame.to_transcription_format();
                    if !retain_within(&mut self.audio, &converted.samples, retain_ms) {
                        tracing::warn!(
                            retain_ms,
                            "audio retention budget reached; skipping acoustic speaker separation \
                             rather than labelling from a partial recording"
                        );
                        self.audio_truncated = true;
                    }
                }
            }

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

        // Patch the WAV header now the length is known. Taken out of `self` so a pipeline reused
        // for a second meeting cannot append to the first meeting's file.
        if let Some(writer) = self.retain.take() {
            let path = writer.path().to_path_buf();
            match writer.finish() {
                Ok(bytes) => stats.retained_audio = Some((path, bytes)),
                Err(e) => {
                    // The transcript is stored and is what the product is for. A header that was
                    // never patched leaves a file `audio_capture::repair` can fix, so this is
                    // reported and not fatal.
                    tracing::warn!(error = %e, "could not finalise the retained audio file");
                }
            }
        }

        tracing::info!(
            frames = stats.frames_processed,
            segments = stats.segments_stored,
            speakers = stats.speakers_detected,
            retained = stats.retained_audio.is_some(),
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

        let labelled = match &self.diarizer {
            ChannelDiarizer::Transcript(diarizer) => diarizer.diarize(&transcript)?,

            // The budget was exceeded mid-recording. The buffer is already gone and a warning was
            // logged; labelling from a prefix is the one thing not to do here.
            ChannelDiarizer::Audio { .. } if self.audio_truncated => return Ok((0, 0)),

            ChannelDiarizer::Audio { diarizer, .. } => {
                diarizer.diarize(&transcript, &self.audio, RETENTION_SAMPLE_RATE)?
            }
        };

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
    diarizer: Option<ChannelDiarizer>,
    /// This channel's audio, retained only for [`ChannelDiarizer::Audio`].
    audio: Vec<f32>,
    /// Set when retention hit its budget. The partial buffer is dropped and the acoustic pass
    /// skipped — see [`ChannelInput::retain`].
    audio_truncated: bool,
    exhausted: bool,
    audio_ms: i64,
    segments: usize,
}

/// How one channel's speakers get separated after recording.
///
/// The two variants differ in what they need, not just in how they work, and the difference is
/// expensive: one needs the channel's entire audio held in memory. Making them separate variants
/// rather than two optional fields means a caller cannot ask for both and cannot forget the cost.
enum ChannelDiarizer {
    /// Needs only the transcript. [`notewise_diarization::TimelineDiarizer`] is the case that
    /// matters: the platform already knows who spoke, so no audio is required.
    Transcript(Box<dyn Diarizer + Send>),

    /// Needs this channel's audio, and therefore a memory budget.
    Audio {
        diarizer: Box<dyn AudioDiarizer + Send>,
        retain_ms: i64,
    },
}

impl std::fmt::Debug for ChannelDiarizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transcript(d) => f.debug_tuple("Transcript").field(&d.name()).finish(),
            Self::Audio {
                diarizer,
                retain_ms,
            } => f
                .debug_struct("Audio")
                .field("diarizer", &diarizer.name())
                .field("retain_ms", retain_ms)
                .finish(),
        }
    }
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
            audio: Vec::new(),
            audio_truncated: false,
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
        self.diarizer = Some(ChannelDiarizer::Transcript(diarizer));
        self
    }

    /// Split this channel's speakers using its audio, retaining at most `retain_ms` of it.
    ///
    /// This is how [`notewise_diarization::EmbeddingDiarizer`] and
    /// [`notewise_diarization::NamedClusterDiarizer`] reach the live recording path. The latter is
    /// the best separation available — acoustic boundaries with platform names — and it needs both
    /// the audio and a timeline.
    ///
    /// # Why the budget is a required argument
    ///
    /// Clustering cannot start until the whole channel is known, so the audio has to be kept.
    /// Mono 16 kHz `f32` is 64 KB per second: about 230 MB for an hour, per channel. That is not a
    /// cost to inherit from a default, so there is no default — a caller enabling this states what
    /// it is willing to spend.
    ///
    /// # What happens at the budget
    ///
    /// Retention stops, the partial buffer is **dropped**, and the acoustic pass is skipped with a
    /// warning; the channel keeps its channel label. Diarizing a prefix would be worse than not
    /// diarizing: every segment past the cutoff would be labelled from
    /// `nearest_cluster` against audio that was never examined, and with
    /// [`notewise_diarization::NamedClusterDiarizer`] those labels are people's names.
    ///
    /// Pair it with [`Self::with_diarizer`]'s argument instead when the platform supplies a
    /// timeline — that path needs no audio at all and so has no budget to blow.
    pub fn with_audio_diarizer(
        mut self,
        diarizer: Box<dyn AudioDiarizer + Send>,
        retain_ms: i64,
    ) -> Self {
        self.diarizer = Some(ChannelDiarizer::Audio {
            diarizer,
            retain_ms,
        });
        self
    }

    /// Keep a frame's samples for the acoustic pass, within budget.
    fn retain(&mut self, samples: &[f32], retain_ms: i64) {
        if self.audio_truncated {
            return;
        }

        if !retain_within(&mut self.audio, samples, retain_ms) {
            tracing::warn!(
                channel = ?self.channel,
                retain_ms,
                "audio retention budget reached; skipping acoustic speaker separation for this \
                 channel rather than labelling from a partial recording"
            );
            self.audio_truncated = true;
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

                // Retain before feeding, and always from the transcription format, so a span's
                // millisecond bounds map to sample offsets at one known rate.
                if let Some(ChannelDiarizer::Audio { retain_ms, .. }) = input.diarizer {
                    let converted = frame.to_transcription_format();
                    input.retain(&converted.samples, retain_ms);
                }

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
            match &input.diarizer {
                None => continue,

                Some(ChannelDiarizer::Transcript(diarizer)) => {
                    refined += refine_channel(db, meeting_id, input.channel, |transcript| {
                        Ok(diarizer.diarize(transcript)?)
                    })?;
                }

                // The budget was exceeded mid-recording. `retain` has already warned and dropped
                // the buffer; labelling from a prefix is the one thing not to do here.
                Some(ChannelDiarizer::Audio { .. }) if input.audio_truncated => continue,

                Some(ChannelDiarizer::Audio { diarizer, .. }) => {
                    refined += refine_channel(db, meeting_id, input.channel, |transcript| {
                        Ok(diarizer.diarize(transcript, &input.audio, RETENTION_SAMPLE_RATE)?)
                    })?;
                }
            }
        }

        Ok(refined)
    }
}

/// Re-label one channel's stored segments, after the recording has already finished.
///
/// # Why this exists separately from [`ChannelInput::with_diarizer`]
///
/// That takes its diarizer when the pipeline is *built*, which is the right shape when the
/// evidence is already in hand — an import, or a timeline captured earlier. It cannot serve a
/// browser extension reporting who is speaking *during* the meeting: at pipeline-construction time
/// that timeline is empty, and the recorder is deliberately not given shared mutable state to read
/// later.
///
/// So the live path accumulates events elsewhere and calls this when the meeting ends. It is the
/// same pass [`ChannelPipeline`] runs at stop, with the same guarantee: a segment the diarizer
/// declines to label keeps its channel label.
///
/// Returns how many segments were given a more specific speaker.
pub fn refine_channel_speakers(
    db: &Database,
    meeting_id: Id,
    channel: Channel,
    diarizer: &dyn Diarizer,
) -> Result<usize> {
    refine_channel(db, meeting_id, channel, |transcript| {
        Ok(diarizer.diarize(transcript)?)
    })
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
    label_with: impl FnOnce(&Transcript) -> Result<Transcript>,
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

    let labelled = label_with(&transcript)?;
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

    /// The claim spec 11 rests on: with retention on, the audio a transcript came from is on disk
    /// afterwards, readable, and at the rate a millisecond bound can be turned into a sample offset.
    #[tokio::test]
    async fn retained_audio_is_written_and_reads_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("meeting.wav");
        let db = db();
        let id = meeting(&db);

        let mut pipeline = Pipeline::new(Box::new(MockEngine::new()))
            .retaining_audio(&path)
            .expect("retain");

        let stats = pipeline
            .run(&db, id, &mut tone(1000), never_stop())
            .await
            .expect("pipeline");

        let (reported_path, bytes) = stats.retained_audio.expect("retention was on");
        assert_eq!(reported_path, path);
        assert!(bytes > 44, "more than a bare header: {bytes}");
        assert_eq!(
            std::fs::metadata(&path).expect("stat").len(),
            bytes,
            "the reported size must be the file's real size"
        );

        // Readable, and at the transcription rate — which is what makes seeking to a segment's
        // millisecond bound arithmetic rather than guesswork.
        let source = FileSource::open_wav(&path).expect("read back");
        assert_eq!(source.format(), AudioFormat::transcription());

        let mut read = FileSource::open_wav(&path).expect("read back");
        let mut samples = 0usize;
        while let Some(frame) = read.next_frame().expect("frame") {
            samples += frame.samples.len();
        }
        let expected = AudioFormat::transcription().sample_rate.hz() as usize; // one second
        assert!(
            samples.abs_diff(expected) < expected / 10,
            "expected about one second of audio, got {samples} samples"
        );
    }

    #[tokio::test]
    async fn without_retention_nothing_is_written_and_stats_say_so() {
        let db = db();
        let id = meeting(&db);
        let mut pipeline = Pipeline::new(Box::new(MockEngine::new()));

        let stats = pipeline
            .run(&db, id, &mut tone(500), never_stop())
            .await
            .expect("pipeline");

        assert!(
            stats.retained_audio.is_none(),
            "retention is off by default and must leave no trace"
        );
    }

    /// A pipeline reused for a second meeting must not append to the first meeting's file.
    #[tokio::test]
    async fn a_second_run_does_not_append_to_the_first_recording() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("first.wav");
        let db = db();

        let mut pipeline = Pipeline::new(Box::new(MockEngine::new()))
            .retaining_audio(&path)
            .expect("retain");

        let first = meeting(&db);
        let stats = pipeline
            .run(&db, first, &mut tone(500), never_stop())
            .await
            .expect("first");
        let after_first = stats.retained_audio.expect("retained").1;

        let second = meeting(&db);
        let stats = pipeline
            .run(&db, second, &mut tone(500), never_stop())
            .await
            .expect("second");

        assert!(
            stats.retained_audio.is_none(),
            "the writer was consumed by the first run, so a second must retain nothing rather \
             than growing the first meeting's file"
        );
        assert_eq!(
            std::fs::metadata(&path).expect("stat").len(),
            after_first,
            "the first recording must be untouched"
        );
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

    /// Records what audio it was handed, then labels every segment with one voice.
    ///
    /// Stands in for `EmbeddingDiarizer`, which needs the ONNX feature and a downloaded model.
    #[derive(Debug, Clone, Default)]
    struct SpyAudioDiarizer {
        samples_seen: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        rate_seen: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl AudioDiarizer for SpyAudioDiarizer {
        fn name(&self) -> &str {
            "spy"
        }

        fn diarize(
            &self,
            transcript: &Transcript,
            samples: &[f32],
            sample_rate: u32,
        ) -> notewise_diarization::Result<Transcript> {
            use std::sync::atomic::Ordering;
            self.samples_seen.store(samples.len(), Ordering::SeqCst);
            self.rate_seen.store(sample_rate, Ordering::SeqCst);

            Ok(Transcript::new(
                transcript
                    .segments
                    .iter()
                    .map(|s| {
                        let mut s = s.clone();
                        s.speaker = Some("Voice A".to_string());
                        s
                    })
                    .collect(),
            ))
        }
    }

    /// The acoustic path reaches the live recorder, and gets the audio at the rate it expects.
    #[tokio::test]
    async fn an_audio_diarizer_receives_the_channels_retained_audio() {
        use std::sync::atomic::Ordering;

        let db = db();
        let id = meeting(&db);

        let spy = SpyAudioDiarizer::default();
        let remote = PlannedEngine(vec![Segment::new("their words", 0, 1_000)]);

        let mut pipeline = ChannelPipeline::new(vec![ChannelInput::new(
            Channel::System,
            Box::new(tone(1_000)),
            Box::new(remote),
        )
        .with_audio_diarizer(Box::new(spy.clone()), 60_000)])
        .expect("one channel");

        pipeline.run(&db, id, never_stop()).await.expect("pipeline");

        assert!(
            spy.samples_seen.load(Ordering::SeqCst) > 0,
            "the diarizer was handed no audio"
        );
        assert_eq!(
            spy.rate_seen.load(Ordering::SeqCst),
            16_000,
            "retained audio must arrive at the transcription rate, whatever the device produced"
        );

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        assert_eq!(stored[0].speaker.as_deref(), Some("Voice A"));
    }

    /// Blowing the budget must skip the pass, not label from a prefix.
    ///
    /// Labelling a truncated recording would attribute every segment past the cutoff from audio
    /// that was never examined — and under `NamedClusterDiarizer` those labels are people's names.
    #[tokio::test]
    async fn exceeding_the_retention_budget_skips_the_acoustic_pass() {
        use std::sync::atomic::Ordering;

        let db = db();
        let id = meeting(&db);

        let spy = SpyAudioDiarizer::default();
        let remote = PlannedEngine(vec![Segment::new("their words", 0, 1_000)]);

        // One second of tone against a 10 ms budget.
        let mut pipeline = ChannelPipeline::new(vec![ChannelInput::new(
            Channel::System,
            Box::new(tone(1_000)),
            Box::new(remote),
        )
        .with_audio_diarizer(Box::new(spy.clone()), 10)])
        .expect("one channel");

        pipeline.run(&db, id, never_stop()).await.expect("pipeline");

        assert_eq!(
            spy.samples_seen.load(Ordering::SeqCst),
            0,
            "the acoustic pass should not have run at all"
        );

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        assert_eq!(
            stored[0].speaker.as_deref(),
            Some("Others"),
            "the channel keeps its true coarse label when separation is skipped"
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

    // ------------------------------------------- the acoustic pass on a mono recording

    /// The case the channel path cannot serve: one stream, no platform timeline, so who spoke is
    /// only recoverable from the voices. An import is exactly this.
    #[tokio::test]
    async fn a_mono_pipeline_hands_its_retained_audio_to_an_audio_diarizer() {
        use std::sync::atomic::Ordering;

        let db = db();
        let id = meeting(&db);
        let spy = SpyAudioDiarizer::default();
        let samples_seen = std::sync::Arc::clone(&spy.samples_seen);
        let rate_seen = std::sync::Arc::clone(&spy.rate_seen);

        let mut pipeline =
            Pipeline::new(Box::new(MockEngine::new())).with_audio_diarizer(Box::new(spy), 60_000);

        pipeline
            .run(&db, id, &mut tone(1_000), never_stop())
            .await
            .expect("pipeline");

        assert_eq!(
            samples_seen.load(Ordering::SeqCst),
            RETENTION_SAMPLE_RATE as usize,
            "one second of audio should reach the diarizer at the retention rate"
        );
        assert_eq!(rate_seen.load(Ordering::SeqCst), RETENTION_SAMPLE_RATE);

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        assert!(!stored.is_empty());
        assert!(
            stored
                .iter()
                .all(|s| s.speaker.as_deref() == Some("Voice A")),
            "the acoustic labels should have been written: {stored:?}"
        );
    }

    /// Past the budget it must label nothing rather than label from a prefix.
    ///
    /// Every segment after the cutoff would otherwise be attributed by `nearest_cluster` against
    /// audio that was never examined — confident labels derived from silence.
    #[tokio::test]
    async fn exceeding_the_retention_budget_skips_the_acoustic_pass_entirely() {
        let db = db();
        let id = meeting(&db);
        let spy = SpyAudioDiarizer::default();
        let samples_seen = std::sync::Arc::clone(&spy.samples_seen);

        // 100 ms of budget against 2 s of audio.
        let mut pipeline =
            Pipeline::new(Box::new(MockEngine::new())).with_audio_diarizer(Box::new(spy), 100);

        let stats = pipeline
            .run(&db, id, &mut tone(2_000), never_stop())
            .await
            .expect("the recording itself must still succeed");

        assert!(stats.segments_stored > 0, "the transcript is not in doubt");
        assert_eq!(stats.segments_attributed, 0);
        assert_eq!(
            samples_seen.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the diarizer must not have been called at all"
        );

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        assert!(
            stored.iter().all(|s| s.speaker.is_none()),
            "no speaker may be invented from a dropped buffer: {stored:?}"
        );
    }

    /// The two diarizers are one field, so the last one set is the one that runs. Otherwise the
    /// transcript pass would overwrite the acoustic labels it knows nothing about.
    #[tokio::test]
    async fn an_audio_diarizer_replaces_a_transcript_diarizer() {
        let db = db();
        let id = meeting(&db);

        let mut pipeline = Pipeline::new(Box::new(MockEngine::new()))
            .with_diarizer(Box::new(SingleSpeakerDiarizer))
            .with_audio_diarizer(Box::new(SpyAudioDiarizer::default()), 60_000);

        pipeline
            .run(&db, id, &mut tone(500), never_stop())
            .await
            .expect("pipeline");

        let stored = MeetingRepository::new(&db).segments(id).unwrap();
        assert!(
            stored
                .iter()
                .all(|s| s.speaker.as_deref() == Some("Voice A")),
            "the acoustic diarizer should have won: {stored:?}"
        );
    }

    /// Retention costs memory, so it must not happen for a pipeline that will never use it.
    #[tokio::test]
    async fn a_transcript_diarizer_retains_no_audio() {
        let db = db();
        let id = meeting(&db);

        let mut pipeline = Pipeline::new(Box::new(MockEngine::new()));
        pipeline
            .run(&db, id, &mut tone(2_000), never_stop())
            .await
            .expect("pipeline");

        assert!(
            pipeline.audio.is_empty(),
            "the default path held {} samples it can never use",
            pipeline.audio.len()
        );
    }

    #[test]
    fn retention_stops_at_the_budget_and_drops_what_it_had() {
        let mut audio = Vec::new();
        let second = vec![0.0f32; RETENTION_SAMPLE_RATE as usize];

        assert!(
            retain_within(&mut audio, &second, 2_000),
            "first second fits"
        );
        assert_eq!(audio.len(), RETENTION_SAMPLE_RATE as usize);

        assert!(
            !retain_within(&mut audio, &second, 1_500),
            "the second second exceeds a 1.5 s budget"
        );
        assert!(audio.is_empty(), "a prefix must not be kept");
    }

    /// A zero or negative budget is "no audio", not "unlimited".
    #[test]
    fn a_zero_budget_retains_nothing() {
        let mut audio = Vec::new();
        assert!(!retain_within(&mut audio, &[0.0; 16], 0));
        assert!(!retain_within(&mut audio, &[0.0; 16], -1));
        assert!(audio.is_empty());
    }
}
