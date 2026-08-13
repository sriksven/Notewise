use serde::{Deserialize, Serialize};

/// A sample rate in Hz.
///
/// A newtype rather than a bare `u32` because mixing up 16 kHz and 48 kHz produces audio that
/// plays at the wrong speed and transcribes to nonsense — a bug that is much easier to
/// prevent at a type boundary than to diagnose from a garbled transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SampleRate(u32);

impl SampleRate {
    /// What speech recognition models expect. Capture is resampled to this.
    pub const WHISPER: SampleRate = SampleRate(16_000);
    /// Common OS capture rate.
    pub const CD: SampleRate = SampleRate(44_100);
    /// The usual native rate for system audio on macOS and Windows.
    pub const STUDIO: SampleRate = SampleRate(48_000);

    pub const fn from_hz(hz: u32) -> Self {
        SampleRate(hz)
    }

    pub const fn hz(&self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for SampleRate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} Hz", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub sample_rate: SampleRate,
    pub channels: u16,
}

impl AudioFormat {
    pub const fn new(sample_rate: SampleRate, channels: u16) -> Self {
        Self {
            sample_rate,
            channels,
        }
    }

    /// The format transcription engines expect: 16 kHz mono.
    pub const fn transcription() -> Self {
        Self::new(SampleRate::WHISPER, 1)
    }

    pub fn is_mono(&self) -> bool {
        self.channels == 1
    }

    /// Whether audio in this format can go straight to a transcription engine.
    pub fn is_transcription_ready(&self) -> bool {
        *self == Self::transcription()
    }

    /// Bytes per second as `f32` samples.
    pub fn bytes_per_second(&self) -> u64 {
        self.sample_rate.hz() as u64 * self.channels as u64 * std::mem::size_of::<f32>() as u64
    }
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self::transcription()
    }
}

impl std::fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let layout = match self.channels {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            n => format!("{n}ch"),
        };
        write!(f, "{} {}", self.sample_rate, layout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_format_is_what_engines_expect() {
        assert!(AudioFormat::default().is_transcription_ready());
        assert_eq!(AudioFormat::default().sample_rate, SampleRate::WHISPER);
        assert!(AudioFormat::default().is_mono());
    }

    #[test]
    fn studio_audio_is_not_transcription_ready() {
        // 48 kHz stereo is what system capture typically produces; it needs conversion.
        let captured = AudioFormat::new(SampleRate::STUDIO, 2);
        assert!(!captured.is_transcription_ready());
    }

    #[test]
    fn sample_rates_of_different_values_are_not_interchangeable() {
        assert_ne!(SampleRate::WHISPER, SampleRate::STUDIO);
        assert_eq!(SampleRate::from_hz(16_000), SampleRate::WHISPER);
    }

    #[test]
    fn bytes_per_second_accounts_for_channels() {
        let mono = AudioFormat::new(SampleRate::WHISPER, 1);
        let stereo = AudioFormat::new(SampleRate::WHISPER, 2);

        assert_eq!(mono.bytes_per_second(), 16_000 * 4);
        assert_eq!(stereo.bytes_per_second(), mono.bytes_per_second() * 2);
    }

    #[test]
    fn formats_display_readably() {
        assert_eq!(AudioFormat::transcription().to_string(), "16000 Hz mono");
        assert_eq!(
            AudioFormat::new(SampleRate::STUDIO, 2).to_string(),
            "48000 Hz stereo"
        );
        assert_eq!(
            AudioFormat::new(SampleRate::STUDIO, 6).to_string(),
            "48000 Hz 6ch"
        );
    }

    #[test]
    fn formats_round_trip_through_json() {
        let format = AudioFormat::new(SampleRate::STUDIO, 2);
        let json = serde_json::to_string(&format).unwrap();
        assert_eq!(serde_json::from_str::<AudioFormat>(&json).unwrap(), format);
    }
}
