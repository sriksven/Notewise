//! Format conversion.
//!
//! Capture produces whatever the OS gives (typically 48 kHz stereo); transcription engines
//! want 16 kHz mono. These functions bridge that gap and are the most-exercised code in the
//! recording path, which is why they are tested closely.

use crate::format::{AudioFormat, SampleRate};
use crate::AudioFrame;

/// Downmix interleaved multi-channel audio to mono by averaging channels.
///
/// Averaging rather than taking channel 0: on a stereo system tap, one speaker's voice can
/// sit almost entirely in one channel, and discarding the other loses them from the transcript.
pub fn to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }

    let channels = channels as usize;
    samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Resample mono audio by linear interpolation.
///
/// Linear interpolation is not the highest-quality resampler — a windowed-sinc filter would
/// be better — but speech recognition is robust to the aliasing it introduces, and it has no
/// dependencies and constant memory. Worth revisiting if transcription accuracy on
/// downsampled audio ever measures worse than on natively-recorded 16 kHz.
pub fn resample_linear(samples: &[f32], from: SampleRate, to: SampleRate) -> Vec<f32> {
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    if from.hz() == 0 || to.hz() == 0 {
        return Vec::new();
    }

    let ratio = to.hz() as f64 / from.hz() as f64;
    let out_len = ((samples.len() as f64) * ratio).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let position = i as f64 / ratio;
        let index = position.floor() as usize;
        let fraction = (position - index as f64) as f32;

        let current = samples.get(index).copied().unwrap_or(0.0);
        let next = samples.get(index + 1).copied().unwrap_or(current);
        out.push(current + (next - current) * fraction);
    }

    out
}

/// Root-mean-square amplitude, used for silence detection and level meters.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

impl AudioFrame {
    /// Convert this frame to 16 kHz mono, the format transcription engines expect.
    ///
    /// A no-op when the frame is already in that format, so calling it unconditionally in the
    /// capture path costs nothing.
    pub fn to_transcription_format(&self) -> AudioFrame {
        if self.format.is_transcription_ready() {
            return self.clone();
        }

        let mono = to_mono(&self.samples, self.format.channels);
        let resampled = resample_linear(&mono, self.format.sample_rate, SampleRate::WHISPER);

        AudioFrame::new(resampled, AudioFormat::transcription(), self.timestamp_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_audio_passes_through_unchanged() {
        let samples = vec![0.1, 0.2, 0.3];
        assert_eq!(to_mono(&samples, 1), samples);
    }

    #[test]
    fn stereo_is_averaged_not_truncated() {
        // L=1.0, R=0.0 must not become 1.0 (channel 0) or 0.0 (channel 1).
        let stereo = vec![1.0, 0.0, 0.5, 0.5];
        assert_eq!(to_mono(&stereo, 2), vec![0.5, 0.5]);
    }

    #[test]
    fn a_voice_isolated_in_one_channel_survives_the_downmix() {
        // The failure this guards against: a speaker panned hard right vanishing entirely.
        let stereo = vec![0.0, 0.8, 0.0, 0.6];
        let mono = to_mono(&stereo, 2);

        assert!(mono.iter().all(|s| *s > 0.0), "the right channel was lost");
    }

    #[test]
    fn a_partial_trailing_frame_is_discarded_rather_than_misaligning() {
        // chunks_exact drops the remainder; keeping it would shift every later sample.
        let ragged = vec![1.0, 1.0, 0.5];
        assert_eq!(to_mono(&ragged, 2), vec![1.0]);
    }

    #[test]
    fn resampling_to_the_same_rate_is_a_no_op() {
        let samples = vec![0.1, 0.2, 0.3];
        assert_eq!(
            resample_linear(&samples, SampleRate::WHISPER, SampleRate::WHISPER),
            samples
        );
    }

    #[test]
    fn downsampling_shortens_proportionally() {
        // 48 kHz -> 16 kHz is a third of the samples.
        let samples = vec![0.0; 4800];
        let out = resample_linear(&samples, SampleRate::STUDIO, SampleRate::WHISPER);

        assert_eq!(out.len(), 1600);
    }

    #[test]
    fn upsampling_lengthens_proportionally() {
        let samples = vec![0.0; 1600];
        let out = resample_linear(&samples, SampleRate::WHISPER, SampleRate::STUDIO);

        assert_eq!(out.len(), 4800);
    }

    #[test]
    fn resampling_preserves_a_constant_signal() {
        // Interpolating between equal values must not introduce ripple.
        let flat = vec![0.5; 1000];
        let out = resample_linear(&flat, SampleRate::STUDIO, SampleRate::WHISPER);

        assert!(
            out.iter().all(|s| (s - 0.5).abs() < 1e-6),
            "constant signal should stay constant"
        );
    }

    #[test]
    fn resampling_preserves_the_signal_range() {
        let ramp: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        let out = resample_linear(&ramp, SampleRate::STUDIO, SampleRate::WHISPER);

        assert!(out.iter().all(|s| (0.0..=1.0).contains(s)));
    }

    #[test]
    fn empty_input_resamples_to_empty() {
        assert!(resample_linear(&[], SampleRate::STUDIO, SampleRate::WHISPER).is_empty());
    }

    #[test]
    fn rms_of_silence_is_zero() {
        assert_eq!(rms(&[0.0; 100]), 0.0);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn rms_reflects_amplitude() {
        assert!((rms(&[1.0, -1.0, 1.0, -1.0]) - 1.0).abs() < 1e-6);
        assert!(rms(&[0.5; 100]) < rms(&[1.0; 100]));
    }

    #[test]
    fn a_captured_frame_converts_to_transcription_format() {
        // What a real system tap produces: 48 kHz stereo.
        let frame = AudioFrame::new(
            vec![0.5; 9600],
            AudioFormat::new(SampleRate::STUDIO, 2),
            1234,
        );

        let converted = frame.to_transcription_format();

        assert!(converted.format.is_transcription_ready());
        assert_eq!(converted.frame_count(), 1600);
        assert_eq!(converted.duration_ms(), frame.duration_ms());
        assert_eq!(converted.timestamp_ms, 1234, "timestamps must be preserved");
    }

    #[test]
    fn converting_an_already_ready_frame_changes_nothing() {
        let frame = AudioFrame::new(vec![0.1; 1600], AudioFormat::transcription(), 0);
        assert_eq!(frame.to_transcription_format(), frame);
    }
}
