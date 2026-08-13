//! Microphone capture via cpal.
//!
//! Behind the `os-capture` feature. This is the one OS capture path that needs no signed
//! bundle and no screen-recording grant — just the microphone permission, which the OS
//! prompts for on first use. That makes it the difference between a product that records
//! in-person meetings today and one that records nothing.
//!
//! # Why a dedicated thread
//!
//! `cpal::Stream` is not `Send`, so it cannot live inside an [`AudioSource`] that crosses
//! threads. The stream is therefore built and owned on its own thread, which forwards samples
//! over a channel. That also keeps the audio callback off the path of anything slow: the
//! callback only pushes into a queue, and a consumer that stalls cannot block the OS audio
//! thread — which on most platforms causes dropouts rather than merely lag.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::format::{AudioFormat, SampleRate};
use crate::{AudioFrame, AudioSource, CaptureConfig, CaptureError, Result};

/// An available input device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
    pub sample_rate: u32,
    pub channels: u16,
}

/// List input devices, so a UI can offer a picker rather than guessing.
pub fn input_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());

    let devices = host
        .input_devices()
        .map_err(|e| CaptureError::BadFormat(format!("enumerating input devices: {e}")))?;

    let mut out = Vec::new();
    for device in devices {
        let Ok(name) = device.name() else { continue };
        // A device that cannot report a config cannot be recorded from; skip rather than
        // offering the user something that will fail when selected.
        let Ok(config) = device.default_input_config() else {
            continue;
        };

        out.push(DeviceInfo {
            is_default: Some(&name) == default_name.as_ref(),
            name,
            sample_rate: config.sample_rate().0,
            channels: config.channels(),
        });
    }

    Ok(out)
}

/// Live microphone input.
#[derive(Debug)]
pub struct MicrophoneSource {
    receiver: Receiver<Vec<f32>>,
    format: AudioFormat,
    device_name: String,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,

    /// Samples received but not yet returned as a whole frame.
    pending: Vec<f32>,
    samples_per_frame: usize,
    /// Total samples emitted, for timestamps.
    emitted: u64,
}

impl MicrophoneSource {
    /// Open the default input device, or the one named in `config.device`.
    pub fn open(config: &CaptureConfig) -> Result<Self> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (audio_tx, audio_rx) = std::sync::mpsc::channel::<Vec<f32>>();

        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let wanted_device = config.device.clone();

        let worker = std::thread::Builder::new()
            .name("notewise-microphone".into())
            .spawn(move || {
                // The stream is built here and never leaves this thread, which is what makes
                // the !Send type usable at all.
                match build_stream(wanted_device, audio_tx) {
                    Ok((stream, format, name)) => {
                        if ready_tx.send(Ok((format, name))).is_err() {
                            return; // caller gave up
                        }
                        // Keep the stream alive until asked to stop. Dropping it stops capture.
                        while !worker_stop.load(Ordering::Relaxed) {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                        drop(stream);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(CaptureError::Io)?;

        // Wait for the device to open so a permission denial or missing device surfaces
        // here rather than as silence once a meeting has started.
        let (format, device_name) = ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| CaptureError::BadFormat("audio device did not open in time".into()))??;

        let per_channel =
            (format.sample_rate.hz() as u64 * config.frame_ms.max(1) as u64 / 1000) as usize;

        Ok(Self {
            receiver: audio_rx,
            format,
            device_name,
            stop,
            worker: Some(worker),
            pending: Vec::new(),
            samples_per_frame: (per_channel * format.channels.max(1) as usize).max(1),
            emitted: 0,
        })
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Drain whatever the callback has queued.
    fn drain(&mut self) {
        loop {
            match self.receiver.try_recv() {
                Ok(chunk) => self.pending.extend(chunk),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    fn take_frame(&mut self) -> AudioFrame {
        let take = self.samples_per_frame.min(self.pending.len());
        let samples: Vec<f32> = self.pending.drain(..take).collect();

        let frames_elapsed = self.emitted / self.format.channels.max(1) as u64;
        let timestamp_ms =
            (frames_elapsed as i64 * 1000) / self.format.sample_rate.hz().max(1) as i64;
        self.emitted += samples.len() as u64;

        AudioFrame::new(samples, self.format, timestamp_ms)
    }
}

impl AudioSource for MicrophoneSource {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn next_frame(&mut self) -> Result<Option<AudioFrame>> {
        // Block until a whole frame is available, or the device goes away. Bounded so a
        // caller polling for a stop signal is never held for long.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);

        loop {
            self.drain();

            if self.pending.len() >= self.samples_per_frame {
                return Ok(Some(self.take_frame()));
            }

            if self.stop.load(Ordering::Relaxed) {
                // Return the tail rather than discarding it — that is the last thing said.
                return Ok(if self.pending.is_empty() {
                    None
                } else {
                    Some(self.take_frame())
                });
            }

            if std::time::Instant::now() >= deadline {
                // A partial frame is better than stalling the pipeline; silence still
                // needs to advance the clock so timestamps stay honest.
                return Ok(Some(self.take_frame()));
            }

            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn stop(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            // Ignore a panicked worker: the device is released either way, and failing here
            // would mask whatever actually went wrong.
            let _ = worker.join();
        }
        Ok(())
    }

    fn is_realtime(&self) -> bool {
        true
    }
}

impl Drop for MicrophoneSource {
    fn drop(&mut self) {
        // Releases the device even if the caller forgot to stop.
        let _ = self.stop();
    }
}

/// Build the input stream on the calling thread.
fn build_stream(
    wanted: Option<String>,
    sender: std::sync::mpsc::Sender<Vec<f32>>,
) -> Result<(cpal::Stream, AudioFormat, String)> {
    let host = cpal::default_host();

    let device = match &wanted {
        Some(name) => host
            .input_devices()
            .map_err(|e| CaptureError::BadFormat(format!("enumerating devices: {e}")))?
            .find(|d| d.name().map(|n| &n == name).unwrap_or(false))
            .ok_or_else(|| CaptureError::DeviceNotFound(name.clone()))?,
        None => host
            .default_input_device()
            .ok_or_else(|| CaptureError::DeviceNotFound("system default".into()))?,
    };

    let name = device.name().unwrap_or_else(|_| "unknown".into());
    let supported = device
        .default_input_config()
        .map_err(|e| CaptureError::BadFormat(format!("reading device config: {e}")))?;

    let format = AudioFormat::new(
        SampleRate::from_hz(supported.sample_rate().0),
        supported.channels(),
    );
    let stream_config: cpal::StreamConfig = supported.config();

    // Errors on the audio thread are logged rather than propagated: there is nowhere to
    // return them to, and tearing down capture mid-meeting over a recoverable glitch is
    // worse than a gap in the transcript.
    let on_error = |e| tracing::warn!(error = %e, "microphone stream error");

    // Every sample format is converted to f32 here so nothing downstream branches on it.
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &_| {
                let _ = sender.send(data.to_vec());
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &_| {
                let converted = data
                    .iter()
                    .map(|s| *s as f32 / i16::MAX as f32)
                    .collect::<Vec<f32>>();
                let _ = sender.send(converted);
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _: &_| {
                let converted = data
                    .iter()
                    .map(|s| (*s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                    .collect::<Vec<f32>>();
                let _ = sender.send(converted);
            },
            on_error,
            None,
        ),
        other => {
            return Err(CaptureError::BadFormat(format!(
                "unsupported sample format {other:?}"
            )))
        }
    }
    .map_err(|e| {
        // A denied microphone permission surfaces here on macOS.
        let message = e.to_string();
        if message.to_lowercase().contains("permission")
            || message.to_lowercase().contains("denied")
        {
            CaptureError::PermissionDenied { what: "microphone" }
        } else {
            CaptureError::BadFormat(format!("building input stream: {message}"))
        }
    })?;

    stream
        .play()
        .map_err(|e| CaptureError::BadFormat(format!("starting the stream: {e}")))?;

    Ok((stream, format, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_can_be_enumerated_without_opening_one() {
        // Must not fail on a machine with no microphone — a picker needs to render either way.
        let devices = input_devices().expect("enumeration should not error");

        for device in &devices {
            assert!(!device.name.is_empty());
            assert!(device.sample_rate > 0, "{}", device.name);
            assert!(device.channels > 0, "{}", device.name);
        }

        assert!(
            devices.iter().filter(|d| d.is_default).count() <= 1,
            "at most one device can be the default"
        );
    }

    #[test]
    fn a_named_device_that_does_not_exist_is_reported() {
        let config = CaptureConfig {
            device: Some("Definitely Not A Real Microphone".into()),
            ..Default::default()
        };

        match MicrophoneSource::open(&config) {
            Err(CaptureError::DeviceNotFound(name)) => {
                assert!(name.contains("Definitely Not"), "{name}");
            }
            Err(other) => panic!("expected DeviceNotFound, got {other:?}"),
            Ok(_) => panic!("should not have opened a nonexistent device"),
        }
    }

    /// Opens the real default microphone.
    ///
    /// Ignored: needs a device present and, on macOS, a microphone permission grant that a
    /// build process cannot answer. Run with
    /// `cargo test -p notewise-audio-capture --features os-capture -- --ignored --nocapture`
    #[test]
    #[ignore = "requires a microphone and an OS permission grant"]
    fn captures_from_the_default_microphone() {
        let config = CaptureConfig::default();
        let mut source = MicrophoneSource::open(&config).expect("open default microphone");

        println!("device: {}", source.device_name());
        println!("format: {}", source.format());

        let mut frames = 0;
        let mut total_samples = 0;
        for _ in 0..5 {
            if let Some(frame) = source.next_frame().expect("frame") {
                frames += 1;
                total_samples += frame.samples.len();
                println!(
                    "frame {frames}: {} samples, rms {:.4}, t={}ms",
                    frame.samples.len(),
                    crate::rms(&frame.samples),
                    frame.timestamp_ms
                );
            }
        }

        source.stop().expect("stop");

        assert!(frames > 0, "no frames captured");
        assert!(total_samples > 0, "no samples captured");
    }
}
