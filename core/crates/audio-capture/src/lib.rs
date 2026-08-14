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
//! **Microphone capture is implemented** ([`MicrophoneSource`], `os-capture` feature) — it
//! needs only the OS microphone permission, no signed bundle.
//!
//! **System audio is implemented on macOS** ([`SystemAudioSource`], `os-capture` feature),
//! via ScreenCaptureKit. It is written and it compiles, but it cannot be *verified* by a test
//! run: it needs the Screen Recording grant, and TCC issues that against a signed bundle
//! identifier which a `cargo test` binary does not have. Its tests are therefore `#[ignore]`d
//! with that reason and are runnable from a signed build. Windows (WASAPI loopback) and Linux
//! (PipeWire) remain unimplemented and return [`CaptureError::Unsupported`].
//!
//! [`FileSource`] and [`SyntheticSource`] are real and work everywhere, which is what lets
//! the transcription pipeline above be developed and tested with no audio hardware.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod convert;
mod features;
mod format;
#[cfg(feature = "os-capture")]
mod microphone;
mod mixer;
mod permissions;
mod source;
#[cfg(all(feature = "os-capture", target_os = "macos"))]
mod system_audio;
mod vad;

pub use convert::{resample_linear, rms, to_mono};
pub use features::{Fbank, FbankConfig, FbankExtractor, Normalization, WindowType};
pub use format::{AudioFormat, SampleRate};
#[cfg(feature = "os-capture")]
pub use microphone::{input_devices, DeviceInfo, MicrophoneSource};
pub use mixer::{soft_clip, MixedSource, Mixer};
pub use permissions::{permission_status, request_permission, PermissionStatus};
pub use source::{AudioSource, CaptureConfig, CaptureKind, FileSource, SyntheticSource, Waveform};
#[cfg(all(feature = "os-capture", target_os = "macos"))]
pub use system_audio::SystemAudioSource;
pub use vad::{Vad, VadConfig, VadReport};

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

    /// A platform capture SDK refused a request, carrying its own reason.
    ///
    /// Separate from [`CaptureError::Unsupported`], whose reasons are compile-time facts about
    /// the build. This one is a runtime refusal — most often a missing permission — and the
    /// message is written for the person who has to go and grant it.
    #[error("{0}")]
    Platform(String),

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

    /// Human name for this backend, for error messages.
    fn describe(&self) -> &'static str {
        match self {
            OsBackend::CoreAudioInput => "Core Audio microphone capture",
            OsBackend::ScreenCaptureKit => "ScreenCaptureKit system audio capture",
            OsBackend::WasapiLoopback => "WASAPI loopback capture",
            OsBackend::PipeWire => "PipeWire capture",
        }
    }

    /// Why this backend is not usable in the current build.
    ///
    /// Returns `None` only when it would actually work. Whether it *succeeds* is a separate
    /// question from whether this build can attempt it — a macOS backend can be perfectly
    /// available here and still fail at runtime for want of a TCC grant. See
    /// [`permission_status`].
    pub fn unavailable_reason(&self) -> Option<&'static str> {
        if !cfg!(feature = "os-capture") {
            return Some("built without the 'os-capture' feature");
        }
        match self {
            // Implemented, via cpal.
            OsBackend::CoreAudioInput if cfg!(target_os = "macos") => None,
            OsBackend::CoreAudioInput => Some("Core Audio exists only on macOS"),
            // Implemented, via the screencapturekit crate.
            OsBackend::ScreenCaptureKit if cfg!(target_os = "macos") => None,
            OsBackend::ScreenCaptureKit => Some("ScreenCaptureKit exists only on macOS"),
            OsBackend::WasapiLoopback | OsBackend::PipeWire => {
                Some("this backend is not implemented yet")
            }
        }
    }

    /// Whether this build can attempt to open this backend at all.
    pub fn is_available(&self) -> bool {
        self.unavailable_reason().is_none()
    }

    /// Open this backend, or explain why it cannot be opened.
    ///
    /// Deliberately fails loudly. A capture path that silently yields no audio produces an
    /// empty transcript and a user who concludes the product is broken rather than
    /// unpermitted.
    #[cfg_attr(not(feature = "os-capture"), allow(unused_variables))]
    pub fn open(&self, config: &CaptureConfig) -> Result<Box<dyn AudioSource>> {
        if let Some(reason) = self.unavailable_reason() {
            return Err(CaptureError::Unsupported {
                what: self.describe(),
                reason,
            });
        }

        // Only reachable for backends `unavailable_reason` just vouched for, so each arm
        // below is compiled under exactly the cfg that makes its source type exist.
        match self {
            #[cfg(feature = "os-capture")]
            OsBackend::CoreAudioInput => Ok(Box::new(MicrophoneSource::open(config)?)),

            #[cfg(all(feature = "os-capture", target_os = "macos"))]
            OsBackend::ScreenCaptureKit => Ok(Box::new(SystemAudioSource::open(config)?)),

            // Either the feature is off or the backend genuinely has no implementation. The
            // guard above has already returned for every such case; this arm exists so the
            // match is exhaustive under every cfg combination rather than by accident.
            other => Err(CaptureError::Unsupported {
                what: other.describe(),
                reason: "no implementation registered for this build",
            }),
        }
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

    /// An unavailable backend must fail loudly rather than yield silence — a capture path
    /// that quietly produces nothing looks like a broken product.
    ///
    /// This deliberately only opens backends that [`OsBackend::unavailable_reason`] has
    /// already ruled out. Opening an available one would reach for real hardware and, on
    /// macOS, a TCC grant that cannot be obtained headlessly. That the two agree is the
    /// invariant worth testing, and it is checked separately below.
    #[test]
    fn unavailable_backends_fail_loudly_rather_than_yielding_silence() {
        for backend in [
            OsBackend::CoreAudioInput,
            OsBackend::ScreenCaptureKit,
            OsBackend::WasapiLoopback,
            OsBackend::PipeWire,
        ] {
            let Some(reason) = backend.unavailable_reason() else {
                continue;
            };

            let err = backend
                .open(&CaptureConfig::default())
                .expect_err("must not pretend to work");
            match err {
                CaptureError::Unsupported { reason: got, .. } => assert_eq!(
                    got, reason,
                    "open() and unavailable_reason() disagree for {backend:?}"
                ),
                other => panic!("{backend:?} gave {other:?}"),
            }
        }
    }

    /// `open` used to return `Err` unconditionally, including for backends
    /// `unavailable_reason` vouched for — so the two disagreed and the only working paths
    /// were the ones that bypassed `open` entirely. Nothing but this test stops that
    /// returning.
    #[test]
    fn availability_is_reported_consistently() {
        for backend in [
            OsBackend::CoreAudioInput,
            OsBackend::ScreenCaptureKit,
            OsBackend::WasapiLoopback,
            OsBackend::PipeWire,
        ] {
            assert_eq!(
                backend.is_available(),
                backend.unavailable_reason().is_none(),
                "{backend:?} reports availability two different ways"
            );
        }

        // Whatever the host is, at least one arrangement must hold: with capture compiled
        // out nothing is available, and with it compiled in on macOS both mic and system
        // audio are.
        if !cfg!(feature = "os-capture") {
            assert!(
                !OsBackend::CoreAudioInput.is_available()
                    && !OsBackend::ScreenCaptureKit.is_available(),
                "nothing is available without the capture feature"
            );
        } else if cfg!(target_os = "macos") {
            assert!(
                OsBackend::CoreAudioInput.is_available(),
                "microphone capture is implemented on macOS via cpal"
            );
            assert!(
                OsBackend::ScreenCaptureKit.is_available(),
                "system audio is implemented on macOS via screencapturekit"
            );
        }
    }

    /// Backends that genuinely have no implementation must say so even in a capture build,
    /// rather than being reported as available and then failing at the call site.
    #[test]
    fn unimplemented_backends_say_so_even_with_capture_compiled_in() {
        for backend in [OsBackend::WasapiLoopback, OsBackend::PipeWire] {
            let reason = backend
                .unavailable_reason()
                .expect("not implemented anywhere yet");
            assert!(
                reason.contains("not implemented") || reason.contains("feature"),
                "{backend:?}: {reason}"
            );
        }
    }

    /// ScreenCaptureKit is implemented on macOS, so with capture compiled in there is no
    /// *build-time* reason it is unavailable. Whether it will actually capture is a runtime
    /// TCC question, which [`permission_status`] answers and this does not.
    #[test]
    fn screencapturekit_is_available_where_it_exists_and_explains_itself_where_it_does_not() {
        let reason = OsBackend::ScreenCaptureKit.unavailable_reason();

        if cfg!(all(target_os = "macos", feature = "os-capture")) {
            assert!(
                reason.is_none(),
                "system audio is implemented on macOS, but reported: {reason:?}"
            );
        } else {
            let reason = reason.expect("unavailable backends must say why");
            assert!(
                reason.contains("macOS") || reason.contains("feature"),
                "{reason}"
            );
        }
    }

    #[test]
    fn unsupported_errors_name_what_was_attempted() {
        let err = OsBackend::WasapiLoopback
            .open(&CaptureConfig::default())
            .unwrap_err();
        assert!(err.to_string().contains("WASAPI"), "{err}");
    }
}
