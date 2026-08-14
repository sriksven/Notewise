//! System audio capture on macOS, via ScreenCaptureKit.
//!
//! # What this unlocks
//!
//! Everything the machine plays: the other people on a video call. Captured separately from
//! the microphone, it answers "who said this" without a model — the microphone is the person
//! at this desk, the system tap is everyone else. See `notewise_recorder::ChannelPipeline` for
//! what consumes it.
//!
//! # Why ScreenCaptureKit, for audio
//!
//! macOS has no public microphone-style API for recording output. Historically this needed a
//! kernel extension or a virtual audio device the user installs by hand. Since macOS 13,
//! ScreenCaptureKit will deliver system audio on a stream — so the framework named for screen
//! recording is, on this platform, the supported way to record sound.
//!
//! That inheritance is the reason for two things below that otherwise look wrong: the stream
//! is configured with a display and a two-pixel video size (a stream needs a content filter,
//! and the smallest legal frame is the cheapest way to satisfy it), and the permission it
//! demands is **Screen Recording**, not Microphone.
//!
//! # What cannot be verified here
//!
//! The Screen Recording grant is a TCC prompt, and TCC keys on a signed bundle identifier. A
//! `cargo test` binary has no stable one, so the grant cannot be obtained from a test run and
//! this file's tests are `#[ignore]`d with that reason. They are runnable from a signed build.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use screencapturekit::prelude::*;
use screencapturekit::stream::configuration::{AudioChannelCount, AudioSampleRate};

use crate::format::{AudioFormat, SampleRate};
use crate::{AudioFrame, AudioSource, CaptureConfig, CaptureError, Result};

/// Live system audio.
pub struct SystemAudioSource {
    /// Held to keep capture running. Dropping this stops the stream, which is why it is
    /// owned here rather than started and forgotten.
    stream: SCStream,
    receiver: Receiver<Vec<f32>>,
    format: AudioFormat,
    stopped: Arc<AtomicBool>,

    /// Samples received but not yet returned as a whole frame.
    pending: Vec<f32>,
    samples_per_frame: usize,
    /// Total samples emitted, for timestamps.
    emitted: u64,
}

// `SCStream` is not `Debug`, and the queued audio must not end up in a log line.
impl std::fmt::Debug for SystemAudioSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemAudioSource")
            .field("format", &self.format)
            .field("pending_samples", &self.pending.len())
            .field("stopped", &self.stopped.load(Ordering::Relaxed))
            .finish()
    }
}

impl SystemAudioSource {
    /// Start capturing system audio.
    ///
    /// Asks ScreenCaptureKit for 16 kHz mono directly, which is what transcription wants, so
    /// nothing downstream has to resample or fold channels together.
    pub fn open(config: &CaptureConfig) -> Result<Self> {
        let content = SCShareableContent::get().map_err(|e| {
            // Overwhelmingly the cause: Screen Recording has not been granted. Say so, rather
            // than reporting the framework's own wording, which mentions neither.
            CaptureError::Platform(format!(
                "could not read shareable content ({e}). System audio needs the Screen \
                 Recording permission, granted to a signed build in System Settings › \
                 Privacy & Security › Screen Recording."
            ))
        })?;

        let displays = content.displays();
        let display = displays.first().ok_or_else(|| {
            CaptureError::Platform("no display to attach a capture stream to".into())
        })?;

        let filter = SCContentFilter::create()
            .with_display(display)
            .with_excluding_windows(&[])
            .build();

        let sc_config = SCStreamConfiguration::default()
            .with_captures_audio(true)
            // Without this the app records itself, and anything it plays back becomes part of
            // the next transcript.
            .with_excludes_current_process_audio(true)
            .with_sample_rate(AudioSampleRate::Rate16000)
            .with_channel_count(AudioChannelCount::Mono)
            // Video is not wanted. It cannot be switched off, so it is made as small as the
            // API allows and the frames are dropped on arrival.
            .with_width(2)
            .with_height(2);

        let (audio_tx, audio_rx) = std::sync::mpsc::channel::<Vec<f32>>();

        let mut stream = SCStream::new(&filter, &sc_config);
        stream.add_output_handler(
            move |sample: CMSampleBuffer, _| {
                let Some(buffers) = sample.audio_buffer_list() else {
                    return;
                };
                // Mono was requested, so the first buffer is the audio. ScreenCaptureKit
                // delivers non-interleaved 32-bit float.
                let Some(buffer) = buffers.buffer(0) else {
                    return;
                };

                let samples: Vec<f32> = buffer
                    .data()
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect();

                if !samples.is_empty() {
                    // A closed channel means the consumer is gone; the stream is about to be
                    // dropped and there is nothing useful to do about it here.
                    let _ = audio_tx.send(samples);
                }
            },
            SCStreamOutputType::Audio,
        );

        stream.start_capture().map_err(|e| {
            CaptureError::Platform(format!(
                "could not start system audio capture ({e}). This needs the Screen Recording \
                 permission, granted to a signed build."
            ))
        })?;

        let format = AudioFormat::new(SampleRate::WHISPER, 1);
        let samples_per_frame = ((format.sample_rate.hz() as u64 * config.frame_ms.max(1) as u64)
            / 1000)
            .max(1) as usize;

        Ok(Self {
            stream,
            receiver: audio_rx,
            format,
            stopped: Arc::new(AtomicBool::new(false)),
            pending: Vec::new(),
            samples_per_frame,
            emitted: 0,
        })
    }

    /// Drain whatever the capture callback has queued.
    fn drain(&mut self) {
        while let Ok(chunk) = self.receiver.try_recv() {
            self.pending.extend(chunk);
        }
    }

    fn take_frame(&mut self) -> AudioFrame {
        let take = self.samples_per_frame.min(self.pending.len());
        let samples: Vec<f32> = self.pending.drain(..take).collect();

        let timestamp_ms =
            (self.emitted as i64 * 1000) / self.format.sample_rate.hz().max(1) as i64;
        self.emitted += samples.len() as u64;

        AudioFrame::new(samples, self.format, timestamp_ms)
    }
}

impl AudioSource for SystemAudioSource {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn next_frame(&mut self) -> Result<Option<AudioFrame>> {
        // Bounded so a caller polling for a stop signal is never held for long. A silent
        // machine produces no callbacks at all, so this deadline is the normal path rather
        // than an error case — unlike a microphone, which always delivers room tone.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);

        loop {
            self.drain();

            if self.pending.len() >= self.samples_per_frame {
                return Ok(Some(self.take_frame()));
            }

            if self.stopped.load(Ordering::Relaxed) {
                // Return the tail rather than discarding it.
                return Ok(if self.pending.is_empty() {
                    None
                } else {
                    Some(self.take_frame())
                });
            }

            if std::time::Instant::now() >= deadline {
                // Nothing is playing. Emit what there is — possibly an empty frame — so the
                // clock advances and this channel stays aligned with the microphone.
                return Ok(Some(self.take_frame()));
            }

            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn stop(&mut self) -> Result<()> {
        if self.stopped.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        self.stream.stop_capture().map_err(|e| {
            CaptureError::Platform(format!("could not stop system audio capture: {e}"))
        })
    }

    fn is_realtime(&self) -> bool {
        true
    }
}

impl Drop for SystemAudioSource {
    fn drop(&mut self) {
        // Releases the capture even if the caller forgot to stop.
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opening the stream needs a Screen Recording grant, which TCC will only give to a signed
    /// bundle. A test binary has no stable bundle identifier, so this cannot pass headlessly
    /// and must not be allowed to imply that system capture works.
    #[test]
    #[ignore = "requires the Screen Recording TCC grant against a signed bundle"]
    fn system_audio_opens_and_delivers_frames() {
        let mut source =
            SystemAudioSource::open(&CaptureConfig::default()).expect("open system audio");

        assert_eq!(source.format().channels, 1);
        assert_eq!(source.format().sample_rate, SampleRate::WHISPER);

        // Play something while this runs, or every frame is legitimately empty.
        let mut captured = 0usize;
        for _ in 0..20 {
            if let Some(frame) = source.next_frame().expect("frame") {
                captured += frame.samples.len();
            }
        }
        println!("captured {captured} samples of system audio");

        source.stop().expect("stop");
    }

    /// Without the grant, opening must fail with a message naming the permission — not return
    /// a source that silently produces nothing for the length of a meeting.
    #[test]
    #[ignore = "outcome depends on whether this machine has already granted Screen Recording"]
    fn a_missing_grant_is_reported_rather_than_recorded_as_silence() {
        match SystemAudioSource::open(&CaptureConfig::default()) {
            Ok(_) => println!("Screen Recording is granted on this machine"),
            Err(e) => {
                let message = e.to_string();
                assert!(
                    message.contains("Screen Recording"),
                    "the error should name the permission: {message}"
                );
            }
        }
    }
}
