//! Mixing microphone and system audio into one stream.
//!
//! A meeting recording needs both sides: the microphone carries the local participant, the
//! system tap carries everyone else. Transcribing them separately would produce two
//! transcripts that have to be re-interleaved by timestamp, so they are summed here instead.
//!
//! # Why summing is not simply addition
//!
//! Two signals near full scale sum above 1.0 and clip, which sounds like harsh distortion
//! and measurably degrades transcription accuracy. Halving both inputs avoids that but makes
//! a quiet meeting quieter, which is worse most of the time — most meetings are not near full
//! scale, and attenuating them costs signal for a problem that was not going to occur.
//!
//! [`Mixer`] therefore leaves the common case untouched and only compresses when the sum
//! actually approaches full scale. See [`soft_clip`].

use crate::convert::to_mono;
use crate::format::AudioFormat;
use crate::{resample_linear, AudioFrame, AudioSource, Result};

/// Level above which compression begins.
///
/// Below this the mixer is exactly linear, so a normal meeting passes through untouched.
const KNEE: f32 = 0.7;

/// Compress a sample toward full scale instead of clipping at it.
///
/// Linear below [`KNEE`]; above it, the remaining headroom is compressed so the output
/// approaches but never exceeds 1.0. A hard clamp would flatten the waveform's peaks, which
/// is the audible distortion that hurts recognition accuracy.
pub fn soft_clip(sample: f32) -> f32 {
    let magnitude = sample.abs();
    if magnitude <= KNEE {
        return sample;
    }

    let sign = sample.signum();
    let excess = magnitude - KNEE;
    let headroom = 1.0 - KNEE;

    // tanh maps the unbounded excess into the remaining headroom smoothly.
    sign * (KNEE + headroom * (excess / headroom).tanh())
}

/// Mixes two audio streams.
#[derive(Debug, Clone, Copy)]
pub struct Mixer {
    /// Gain applied to the microphone before summing.
    pub mic_gain: f32,
    /// Gain applied to system audio before summing.
    ///
    /// Defaults slightly below the microphone: system audio is usually already normalized by
    /// the conferencing app, while a microphone is subject to how far away someone is sitting.
    pub system_gain: f32,
    /// Format the mix is produced in.
    pub output: AudioFormat,
}

impl Default for Mixer {
    fn default() -> Self {
        Self {
            mic_gain: 1.0,
            system_gain: 0.9,
            output: AudioFormat::transcription(),
        }
    }
}

impl Mixer {
    pub fn new(mic_gain: f32, system_gain: f32) -> Self {
        Self {
            mic_gain: mic_gain.max(0.0),
            system_gain: system_gain.max(0.0),
            output: AudioFormat::transcription(),
        }
    }

    /// Bring a frame's samples into the output format.
    fn normalize(&self, frame: &AudioFrame) -> Vec<f32> {
        let mono = to_mono(&frame.samples, frame.format.channels);
        if frame.format.sample_rate == self.output.sample_rate {
            mono
        } else {
            resample_linear(&mono, frame.format.sample_rate, self.output.sample_rate)
        }
    }

    /// Mix two frames.
    ///
    /// Inputs may differ in sample rate and channel count; both are converted to the output
    /// format first. The shorter frame is treated as silent past its end rather than
    /// truncating the longer one, because dropping audio loses speech.
    pub fn mix(&self, mic: &AudioFrame, system: &AudioFrame) -> AudioFrame {
        let mic_samples = self.normalize(mic);
        let system_samples = self.normalize(system);

        let len = mic_samples.len().max(system_samples.len());
        let mut mixed = Vec::with_capacity(len);

        for i in 0..len {
            let a = mic_samples.get(i).copied().unwrap_or(0.0) * self.mic_gain;
            let b = system_samples.get(i).copied().unwrap_or(0.0) * self.system_gain;
            mixed.push(soft_clip(a + b));
        }

        // The earlier timestamp: the mixed frame starts when its earliest input did.
        AudioFrame::new(
            mixed,
            self.output,
            mic.timestamp_ms.min(system.timestamp_ms),
        )
    }

    /// Mix a single frame — gain and limiting, no second stream.
    ///
    /// Used when only one of the two sources is available, so the signal path is identical
    /// whether or not a system tap is present.
    pub fn passthrough(&self, frame: &AudioFrame, gain: f32) -> AudioFrame {
        let samples = self
            .normalize(frame)
            .into_iter()
            .map(|s| soft_clip(s * gain))
            .collect();

        AudioFrame::new(samples, self.output, frame.timestamp_ms)
    }
}

/// An [`AudioSource`] combining a microphone and a system tap.
///
/// Either may be absent — a user with no system-audio permission still gets a usable
/// recording from the microphone alone, rather than no recording at all.
#[derive(Debug)]
pub struct MixedSource {
    mic: Option<Box<dyn AudioSource>>,
    system: Option<Box<dyn AudioSource>>,
    mixer: Mixer,
}

impl MixedSource {
    pub fn new(
        mic: Option<Box<dyn AudioSource>>,
        system: Option<Box<dyn AudioSource>>,
        mixer: Mixer,
    ) -> Self {
        Self { mic, system, mixer }
    }

    /// Whether any source is attached at all.
    pub fn has_input(&self) -> bool {
        self.mic.is_some() || self.system.is_some()
    }
}

impl AudioSource for MixedSource {
    fn format(&self) -> AudioFormat {
        self.mixer.output
    }

    fn next_frame(&mut self) -> Result<Option<AudioFrame>> {
        let mic_frame = match &mut self.mic {
            Some(source) => source.next_frame()?,
            None => None,
        };
        let system_frame = match &mut self.system {
            Some(source) => source.next_frame()?,
            None => None,
        };

        Ok(match (mic_frame, system_frame) {
            (Some(mic), Some(system)) => Some(self.mixer.mix(&mic, &system)),
            (Some(mic), None) => Some(self.mixer.passthrough(&mic, self.mixer.mic_gain)),
            (None, Some(system)) => Some(self.mixer.passthrough(&system, self.mixer.system_gain)),
            // Both exhausted.
            (None, None) => None,
        })
    }

    fn stop(&mut self) -> Result<()> {
        // Stop both even if the first fails; leaving a device open is worse than the error.
        let mic = self.mic.as_mut().map(|s| s.stop()).unwrap_or(Ok(()));
        let system = self.system.as_mut().map(|s| s.stop()).unwrap_or(Ok(()));
        mic.and(system)
    }

    fn is_realtime(&self) -> bool {
        self.mic.as_ref().is_some_and(|s| s.is_realtime())
            || self.system.as_ref().is_some_and(|s| s.is_realtime())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SampleRate;
    use crate::rms;
    use crate::source::{CaptureConfig, FileSource, SyntheticSource, Waveform};

    fn frame(value: f32, len: usize, format: AudioFormat, ts: i64) -> AudioFrame {
        AudioFrame::new(vec![value; len], format, ts)
    }

    fn mono(value: f32, len: usize) -> AudioFrame {
        frame(value, len, AudioFormat::transcription(), 0)
    }

    #[test]
    fn quiet_signals_pass_through_untouched() {
        // The common case: a normal meeting is nowhere near full scale, and compressing
        // it would cost signal for a problem that was not going to happen.
        for level in [0.0, 0.1, 0.3, 0.5, 0.7] {
            assert!(
                (soft_clip(level) - level).abs() < 1e-6,
                "{level} should be linear"
            );
            assert!((soft_clip(-level) + level).abs() < 1e-6);
        }
    }

    #[test]
    fn loud_signals_are_compressed_not_clipped() {
        // Above the knee the output must keep rising — a hard clamp would flatten it,
        // and flattened peaks are the distortion that hurts recognition.
        let a = soft_clip(0.9);
        let b = soft_clip(1.4);
        let c = soft_clip(2.0);

        assert!(
            a < b && b < c,
            "compression must stay monotonic: {a} {b} {c}"
        );
        assert!(a > 0.7, "should exceed the knee");
    }

    #[test]
    fn nothing_ever_exceeds_full_scale() {
        // The curve approaches 1.0 asymptotically, and `tanh` saturates to exactly 1.0 in
        // f32 well before the input does. Full scale is a valid sample; the requirement is
        // that nothing goes past it, which is what would wrap or clip downstream.
        for level in [1.0, 1.5, 2.0, 10.0, 1000.0] {
            assert!(
                soft_clip(level) <= 1.0,
                "{level} produced {}",
                soft_clip(level)
            );
            assert!(soft_clip(-level) >= -1.0);
        }

        // And a normal overload is still strictly inside the range, not pinned at the top.
        assert!(soft_clip(1.2) < 1.0);
    }

    #[test]
    fn soft_clipping_is_symmetric() {
        for level in [0.2, 0.8, 1.5] {
            assert!((soft_clip(level) + soft_clip(-level)).abs() < 1e-6);
        }
    }

    #[test]
    fn silence_plus_silence_is_silence() {
        let mixed = Mixer::default().mix(&mono(0.0, 100), &mono(0.0, 100));
        assert_eq!(rms(&mixed.samples), 0.0);
    }

    #[test]
    fn one_silent_side_leaves_the_other_essentially_intact() {
        // Someone on mute should not make everyone else quieter.
        let mixer = Mixer::new(1.0, 1.0);
        let mixed = mixer.mix(&mono(0.4, 100), &mono(0.0, 100));

        assert!(
            (rms(&mixed.samples) - 0.4).abs() < 1e-3,
            "got {}",
            rms(&mixed.samples)
        );
    }

    #[test]
    fn both_sides_speaking_stays_within_range() {
        // Two people talking over each other is exactly when naive summing clips.
        let mixed = Mixer::new(1.0, 1.0).mix(&mono(0.8, 100), &mono(0.8, 100));

        assert!(
            mixed.samples.iter().all(|s| s.abs() < 1.0),
            "mix exceeded full scale"
        );
        assert!(
            rms(&mixed.samples) > 0.8,
            "the mix should still be louder than either input"
        );
    }

    #[test]
    fn gains_are_applied_per_source() {
        let muted_mic = Mixer::new(0.0, 1.0).mix(&mono(0.5, 50), &mono(0.2, 50));
        assert!((rms(&muted_mic.samples) - 0.2).abs() < 1e-3);

        let muted_system = Mixer::new(1.0, 0.0).mix(&mono(0.5, 50), &mono(0.2, 50));
        assert!((rms(&muted_system.samples) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn negative_gain_is_clamped_to_zero() {
        // A negative gain would invert the signal and partially cancel the other source.
        let mixer = Mixer::new(-2.0, 1.0);
        assert_eq!(mixer.mic_gain, 0.0);
    }

    #[test]
    fn inputs_in_different_formats_are_normalized_before_summing() {
        // The realistic case: a 48kHz stereo system tap and a 16kHz mono microphone.
        let mic = frame(0.3, 1600, AudioFormat::transcription(), 0);
        let system = frame(0.3, 9600, AudioFormat::new(SampleRate::STUDIO, 2), 0);

        let mixed = Mixer::new(1.0, 1.0).mix(&mic, &system);

        assert!(mixed.format.is_transcription_ready());
        assert_eq!(mixed.frame_count(), 1600, "both are one second of audio");
        assert!(rms(&mixed.samples) > 0.5, "both sources should be present");
    }

    #[test]
    fn a_shorter_frame_does_not_truncate_the_longer_one() {
        // Truncating would silently drop speech from whichever side ran longer.
        let mixed = Mixer::new(1.0, 1.0).mix(&mono(0.5, 100), &mono(0.5, 40));

        assert_eq!(mixed.samples.len(), 100);
        assert!(mixed.samples[90] != 0.0, "the tail must survive");
    }

    #[test]
    fn the_mixed_timestamp_is_the_earlier_input() {
        let mic = frame(0.1, 10, AudioFormat::transcription(), 5000);
        let system = frame(0.1, 10, AudioFormat::transcription(), 4200);

        assert_eq!(Mixer::default().mix(&mic, &system).timestamp_ms, 4200);
    }

    #[test]
    fn a_mixed_source_combines_two_streams() {
        let config = CaptureConfig::default();
        let mut source = MixedSource::new(
            Some(Box::new(SyntheticSource::new(
                Waveform::Sine { hz: 440 },
                500,
                &config,
            ))),
            Some(Box::new(SyntheticSource::new(
                Waveform::Sine { hz: 880 },
                500,
                &config,
            ))),
            Mixer::default(),
        );

        let mut frames = 0;
        while let Some(frame) = source.next_frame().unwrap() {
            assert!(frame.format.is_transcription_ready());
            assert!(frame.samples.iter().all(|s| s.abs() <= 1.0));
            frames += 1;
        }

        assert_eq!(frames, 5);
        assert!(source.has_input());
    }

    #[test]
    fn a_microphone_alone_still_records() {
        // A user without system-audio permission must get a usable recording, not none.
        let config = CaptureConfig::default();
        let mut source = MixedSource::new(
            Some(Box::new(SyntheticSource::new(
                Waveform::Sine { hz: 440 },
                300,
                &config,
            ))),
            None,
            Mixer::default(),
        );

        let frame = source.next_frame().unwrap().expect("should produce audio");
        assert!(rms(&frame.samples) > 0.1);
    }

    #[test]
    fn system_audio_alone_still_records() {
        let config = CaptureConfig::default();
        let mut source = MixedSource::new(
            None,
            Some(Box::new(SyntheticSource::new(
                Waveform::Sine { hz: 440 },
                300,
                &config,
            ))),
            Mixer::default(),
        );

        assert!(source.next_frame().unwrap().is_some());
    }

    #[test]
    fn a_source_with_no_inputs_yields_nothing_rather_than_hanging() {
        let mut source = MixedSource::new(None, None, Mixer::default());

        assert!(!source.has_input());
        assert!(source.next_frame().unwrap().is_none());
    }

    #[test]
    fn one_stream_ending_early_does_not_end_the_recording() {
        // The system tap can stop when a call ends while the microphone keeps running.
        let config = CaptureConfig::default();
        let mut source = MixedSource::new(
            Some(Box::new(SyntheticSource::new(
                Waveform::Sine { hz: 440 },
                500,
                &config,
            ))),
            Some(Box::new(SyntheticSource::new(
                Waveform::Sine { hz: 880 },
                200,
                &config,
            ))),
            Mixer::default(),
        );

        let mut frames = 0;
        while source.next_frame().unwrap().is_some() {
            frames += 1;
        }

        assert_eq!(frames, 5, "the longer stream should determine the length");
    }

    #[test]
    fn a_file_backed_mix_is_not_realtime() {
        let mut source = MixedSource::new(
            Some(Box::new(FileSource::from_samples(
                vec![0.1; 100],
                AudioFormat::transcription(),
                100,
            ))),
            None,
            Mixer::default(),
        );

        assert!(!source.is_realtime());
        assert!(source.stop().is_ok());
    }

    #[test]
    fn the_default_favours_the_microphone_slightly() {
        // System audio arrives already normalized by the conferencing app; a microphone
        // depends on how far away someone is sitting.
        let mixer = Mixer::default();
        assert!(mixer.mic_gain >= mixer.system_gain);
    }
}
