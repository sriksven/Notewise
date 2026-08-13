//! Cross-platform audio capture.
//!
//! Defines one interface — give me a stream of audio frames — with per-OS implementations
//! underneath. Nothing above this crate knows which OS it is running on, or whether the audio
//! came from a microphone, a system loopback tap, or a file.
//!
//! # What is and is not implemented here
//!
//! The interface, the frame and format types, and the conversion helpers ([`to_mono`],
//! [`resample_linear`]) are complete and tested. The **OS capture backends are behind the
//! `os-capture` feature and are not implemented**: each needs a platform audio SDK, and on
//! macOS system-audio capture additionally requires ScreenCaptureKit permission granted to a
//! signed bundle — a TCC prompt that cannot be answered by a build process. Calling one
//! returns [`CaptureError::Unsupported`] with a specific reason rather than silently
//! producing nothing.
//!
//! [`FileSource`] and [`SyntheticSource`] are real and work everywhere, which is what lets
//! the transcription pipeline above be developed and tested with no audio hardware.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod convert;
mod format;
mod mixer;
mod source;

pub use convert::{resample_linear, rms, to_mono};
pub use mixer::{soft_clip, MixedSource, Mixer};
pub use format::{AudioFormat, SampleRate};
pub use source::{AudioSource, CaptureConfig, CaptureKind, FileSource, SyntheticSource, Waveform};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("{what} is not available on this build: {reason}")]
    Unsupported {
        what: &'static str,
        reason: &'static str,
    },

    #[error("permission to capture {what} was not granted")]
    PermissionDenied { what: &'static str },

    #[error("no audio device matching '{0}'")]
    DeviceNotFound(String),

    #[error("unsupported audio format: {0}")]
    BadFormat(String),

    #[error("capture already running")]
    AlreadyRunning,

    #[error("capture is not running")]
    NotRunning,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, CaptureError>;

/// A chunk of captured audio.
///
/// Samples are interleaved `f32` in `-1.0..=1.0`. Every engine downstream takes `f32`, so
/// converting once at the boundary avoids a format matrix further up.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioFrame {
    pub samples: Vec<f32>,
    pub format: AudioFormat,
    /// Milliseconds since capture started. The transcript's time base.
    pub timestamp_ms: i64,
}

impl AudioFrame {
    pub fn new(samples: Vec<f32>, format: AudioFormat, timestamp_ms: i64) -> Self {
        Self {
            samples,
            format,
            timestamp_ms,
        }
    }

    /// Samples per channel.
    pub fn frame_count(&self) -> usize {
        if self.format.channels == 0 {
            return 0;
        }
        self.samples.len() / self.format.channels as usize
    }

    /// Duration of this frame in milliseconds.
    pub fn duration_ms(&self) -> i64 {
        if self.format.sample_rate.hz() == 0 {
            return 0;
        }
        (self.frame_count() as i64 * 1000) / self.format.sample_rate.hz() as i64
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// Backends that would exist with the `os-capture` feature.
///
/// Listed here so the constraint is visible in the type system rather than only in prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsBackend {
    /// macOS microphone via Core Audio.
    CoreAudioInput,
    /// macOS system audio via ScreenCaptureKit. Needs a TCC grant against a signed bundle.
    ScreenCaptureKit,
    /// Windows loopback via WASAPI.
    WasapiLoopback,
    /// Linux via PipeWire.
    PipeWire,
}

impl OsBackend {
    /// The backend appropriate to the host, if one exists.
    pub fn for_host(kind: CaptureKind) -> Option<Self> {
        match (std::env::consts::OS, kind) {
            ("macos", CaptureKind::Microphone) => Some(OsBackend::CoreAudioInput),
            ("macos", CaptureKind::SystemAudio) => Some(OsBackend::ScreenCaptureKit),
            ("windows", CaptureKind::SystemAudio) => Some(OsBackend::WasapiLoopback),
            ("linux", _) => Some(OsBackend::PipeWire),
            _ => None,
        }
    }

    /// Why this backend is not usable in the current build.
    ///
    /// Returns `None` only when it would actually work.
    pub fn unavailable_reason(&self) -> Option<&'static str> {
        if !cfg!(feature = "os-capture") {
            return Some("built without the 'os-capture' feature");
        }
        match self {
            OsBackend::ScreenCaptureKit => Some(
                "ScreenCaptureKit requires a screen-recording permission grant against a \
                 signed application bundle",
            ),
            _ => Some("this backend is not implemented yet"),
        }
    }

    /// Open this backend, or explain why it cannot be opened.
    ///
    /// Deliberately fails loudly. A capture path that silently yields no audio produces an
    /// empty transcript and a user who thinks the product is broken rather than unpermitted.
    pub fn open(&self, _config: &CaptureConfig) -> Result<Box<dyn AudioSource>> {
        let reason = self
            .unavailable_reason()
            .unwrap_or("no implementation registered");

        Err(CaptureError::Unsupported {
            what: match self {
                OsBackend::CoreAudioInput => "Core Audio microphone capture",
                OsBackend::ScreenCaptureKit => "ScreenCaptureKit system audio capture",
                OsBackend::WasapiLoopback => "WASAPI loopback capture",
                OsBackend::PipeWire => "PipeWire capture",
            },
            reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(samples: usize, channels: u16, rate: u32) -> AudioFrame {
        AudioFrame::new(
            vec![0.0; samples],
            AudioFormat::new(SampleRate::from_hz(rate), channels),
            0,
        )
    }

    #[test]
    fn frame_count_accounts_for_interleaving() {
        assert_eq!(frame(1600, 1, 16_000).frame_count(), 1600);
        assert_eq!(frame(1600, 2, 16_000).frame_count(), 800);
    }

    #[test]
    fn duration_is_computed_from_the_sample_rate() {
        assert_eq!(frame(16_000, 1, 16_000).duration_ms(), 1000);
        assert_eq!(frame(8_000, 1, 16_000).duration_ms(), 500);
        // Stereo: same wall-clock duration, twice the samples.
        assert_eq!(frame(32_000, 2, 16_000).duration_ms(), 1000);
    }

    #[test]
    fn a_zero_channel_frame_does_not_divide_by_zero() {
        assert_eq!(frame(100, 0, 16_000).frame_count(), 0);
        assert_eq!(frame(100, 0, 16_000).duration_ms(), 0);
    }

    #[test]
    fn host_backend_selection_matches_the_platform() {
        let mic = OsBackend::for_host(CaptureKind::Microphone);
        match std::env::consts::OS {
            "macos" => assert_eq!(mic, Some(OsBackend::CoreAudioInput)),
            "linux" => assert_eq!(mic, Some(OsBackend::PipeWire)),
            _ => {}
        }
    }

    #[test]
    fn os_backends_fail_loudly_rather_than_yielding_silence() {
        // A capture path that silently produces nothing looks like a broken product.
        for backend in [
            OsBackend::CoreAudioInput,
            OsBackend::ScreenCaptureKit,
            OsBackend::WasapiLoopback,
            OsBackend::PipeWire,
        ] {
            let err = backend
                .open(&CaptureConfig::default())
                .expect_err("must not pretend to work");
            assert!(matches!(err, CaptureError::Unsupported { .. }), "{err:?}");
        }
    }

    #[test]
    fn screencapturekit_explains_the_permission_requirement() {
        let reason = OsBackend::ScreenCaptureKit.unavailable_reason().unwrap();
        assert!(
            reason.contains("permission") || reason.contains("feature"),
            "{reason}"
        );
    }

    #[test]
    fn unsupported_errors_name_what_was_attempted() {
        let err = OsBackend::WasapiLoopback
            .open(&CaptureConfig::default())
            .unwrap_err();
        assert!(err.to_string().contains("WASAPI"), "{err}");
    }
}
