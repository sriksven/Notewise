//! Mel filterbank features.
//!
//! Speech models do not take waveforms. Speaker-embedding networks and most ASR encoders take
//! a **log-mel filterbank** — a time-frequency image on a perceptual frequency scale — and the
//! extraction has to match what the model was trained on almost exactly. A filterbank that is
//! off by a window function or a normalisation step does not fail loudly; it produces
//! plausible-looking features and quietly worse results, which is the hardest kind of bug to
//! find later.
//!
//! This implements the Kaldi convention, because that is what the WeSpeaker, 3D-Speaker and
//! NeMo model families were trained with:
//!
//! 1. DC offset removed per frame
//! 2. Pre-emphasis, `x[n] - 0.97·x[n-1]`, applied per frame against the *original* previous
//!    sample rather than the already-filtered one
//! 3. Povey window — Hann raised to 0.85, Kaldi's default
//! 4. Power spectrum from a real FFT, padded to the next power of two
//! 5. Triangular mel filters, equally spaced on the mel scale
//! 6. Natural log, floored to avoid `log(0)`
//!
//! The two conventions that most often get silently mismatched are the window (Povey, not
//! Hamming) and the log base (natural, not base 10). Both are asserted in the tests below.

use rustfft::num_complex::Complex32;
use rustfft::FftPlanner;

/// How to compute the filterbank.
///
/// The defaults are Kaldi's defaults, which is what the supported models expect. Changing one
/// without changing the model is a silent accuracy regression, not an error.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FbankConfig {
    pub sample_rate: u32,
    /// Analysis window length in milliseconds.
    pub frame_length_ms: f32,
    /// Hop between frames in milliseconds.
    pub frame_shift_ms: f32,
    /// Number of mel filters. 80 for every model this crate targets.
    pub num_mel_bins: usize,
    pub low_freq_hz: f32,
    /// Upper edge. Negative means "Nyquist plus this", Kaldi's convention for trimming the
    /// top band — `-400` at 16 kHz gives 7600 Hz.
    pub high_freq_hz: f32,
    pub preemphasis: f32,
    /// Multiplied into every sample before analysis.
    ///
    /// Kaldi computes filterbanks over **int16-scaled** audio (-32768..32767), not the
    /// -1.0..1.0 floats the rest of this codebase uses. Every model trained through Kaldi's
    /// pipeline — WeSpeaker, 3D-Speaker, NeMo — therefore saw features about `2·ln(32768)`
    /// ≈ 20.8 larger than the same audio produces here. Cepstral mean normalisation cancels a
    /// constant offset, which is why the mismatch hides when it is on and bites when it is off.
    pub input_scale: f32,
    /// Subtract each bin's mean over time. WeSpeaker and 3D-Speaker apply this; skipping it
    /// leaves a channel/recording bias in the embedding that clustering then reads as speaker
    /// identity.
    pub mean_normalize: bool,
}

impl Default for FbankConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16_000,
            frame_length_ms: 25.0,
            frame_shift_ms: 10.0,
            num_mel_bins: 80,
            low_freq_hz: 20.0,
            high_freq_hz: -400.0,
            preemphasis: 0.97,
            input_scale: 32_768.0,
            mean_normalize: true,
        }
    }
}

impl FbankConfig {
    pub fn frame_length(&self) -> usize {
        (self.sample_rate as f32 * self.frame_length_ms / 1000.0).round() as usize
    }

    pub fn frame_shift(&self) -> usize {
        (self.sample_rate as f32 * self.frame_shift_ms / 1000.0).round() as usize
    }

    /// FFT size: the next power of two at or above the frame length.
    pub fn fft_size(&self) -> usize {
        self.frame_length().next_power_of_two()
    }

    fn resolved_high_freq(&self) -> f32 {
        let nyquist = self.sample_rate as f32 / 2.0;
        if self.high_freq_hz <= 0.0 {
            nyquist + self.high_freq_hz
        } else {
            self.high_freq_hz.min(nyquist)
        }
    }
}

/// A `frames × num_mel_bins` log-mel filterbank.
#[derive(Debug, Clone, PartialEq)]
pub struct Fbank {
    /// Row-major: `data[frame * num_bins + bin]`.
    pub data: Vec<f32>,
    pub frames: usize,
    pub num_bins: usize,
}

impl Fbank {
    pub fn frame(&self, index: usize) -> &[f32] {
        let start = index * self.num_bins;
        &self.data[start..start + self.num_bins]
    }

    pub fn is_empty(&self) -> bool {
        self.frames == 0
    }
}

/// Computes log-mel filterbanks.
///
/// Holds the FFT plan and the mel filter matrix, both of which are expensive to build and
/// constant for a given config. Reuse one across a meeting rather than constructing per call.
#[derive(Clone)]
pub struct FbankExtractor {
    config: FbankConfig,
    window: Vec<f32>,
    /// Per filter: the first bin it touches, and its weights from there.
    filters: Vec<(usize, Vec<f32>)>,
    planner: std::sync::Arc<dyn rustfft::Fft<f32>>,
}

impl std::fmt::Debug for FbankExtractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The FFT plan and filter matrix are large and say nothing useful in a log.
        f.debug_struct("FbankExtractor")
            .field("config", &self.config)
            .field("filters", &self.filters.len())
            .finish()
    }
}

impl FbankExtractor {
    pub fn new(config: FbankConfig) -> Self {
        let frame_length = config.frame_length();
        let fft_size = config.fft_size();

        let mut planner = FftPlanner::new();
        Self {
            window: povey_window(frame_length),
            filters: mel_filters(&config, fft_size),
            planner: planner.plan_fft_forward(fft_size),
            config,
        }
    }

    pub fn config(&self) -> &FbankConfig {
        &self.config
    }

    /// Compute the filterbank for mono samples.
    ///
    /// Audio shorter than one frame yields no frames rather than a zero-padded one: a padded
    /// frame is mostly silence, and a model asked to embed mostly-silence returns a confident
    /// vector for nothing.
    pub fn compute(&self, samples: &[f32]) -> Fbank {
        let frame_length = self.config.frame_length();
        let frame_shift = self.config.frame_shift();
        let num_bins = self.config.num_mel_bins;

        if samples.len() < frame_length || frame_shift == 0 {
            return Fbank {
                data: Vec::new(),
                frames: 0,
                num_bins,
            };
        }

        let frames = (samples.len() - frame_length) / frame_shift + 1;
        let fft_size = self.config.fft_size();

        let mut data = Vec::with_capacity(frames * num_bins);
        let mut buffer = vec![Complex32::new(0.0, 0.0); fft_size];
        let mut frame = vec![0.0f32; frame_length];

        for f in 0..frames {
            let start = f * frame_shift;
            frame.copy_from_slice(&samples[start..start + frame_length]);

            if self.config.input_scale != 1.0 {
                for sample in frame.iter_mut() {
                    *sample *= self.config.input_scale;
                }
            }

            // Kaldi removes the DC offset per frame, before pre-emphasis.
            let mean = frame.iter().sum::<f32>() / frame_length as f32;
            for sample in frame.iter_mut() {
                *sample -= mean;
            }

            // Pre-emphasis, applied right to left so each step uses the *original* previous
            // sample. Left to right would feed already-filtered values back in and apply the
            // filter cumulatively.
            if self.config.preemphasis != 0.0 {
                for i in (1..frame_length).rev() {
                    frame[i] -= self.config.preemphasis * frame[i - 1];
                }
                // Kaldi treats the first sample as its own predecessor.
                frame[0] -= self.config.preemphasis * frame[0];
            }

            for (i, sample) in frame.iter().enumerate() {
                buffer[i] = Complex32::new(sample * self.window[i], 0.0);
            }
            buffer[frame_length..].fill(Complex32::new(0.0, 0.0));

            self.planner.process(&mut buffer);

            // Only the non-redundant half of a real signal's spectrum carries information.
            let power: Vec<f32> = buffer[..fft_size / 2 + 1]
                .iter()
                .map(|c| c.re * c.re + c.im * c.im)
                .collect();

            for (offset, weights) in &self.filters {
                let energy: f32 = weights
                    .iter()
                    .enumerate()
                    .map(|(i, w)| w * power.get(offset + i).copied().unwrap_or(0.0))
                    .sum();

                // Natural log, floored. Kaldi uses ln, not log10; a base mismatch scales every
                // feature by 2.3 and the model silently underperforms.
                data.push(energy.max(f32::EPSILON).ln());
            }
        }

        let mut fbank = Fbank {
            data,
            frames,
            num_bins,
        };

        if self.config.mean_normalize {
            mean_normalize(&mut fbank);
        }
        fbank
    }
}

/// Subtract each bin's mean across time.
///
/// Removes the constant part of the channel — microphone response, room, gain — which is
/// otherwise a strong, speaker-independent signal that clustering happily latches onto.
fn mean_normalize(fbank: &mut Fbank) {
    if fbank.frames == 0 {
        return;
    }

    let mut means = vec![0.0f32; fbank.num_bins];
    for frame in fbank.data.chunks_exact(fbank.num_bins) {
        for (mean, value) in means.iter_mut().zip(frame) {
            *mean += value;
        }
    }
    for mean in means.iter_mut() {
        *mean /= fbank.frames as f32;
    }

    for frame in fbank.data.chunks_exact_mut(fbank.num_bins) {
        for (value, mean) in frame.iter_mut().zip(&means) {
            *value -= mean;
        }
    }
}

/// Kaldi's default window: Hann raised to 0.85.
///
/// Not Hamming. The difference is small in a plot and consistent in its effect on features,
/// which is exactly why a mismatch here is so easy to ship.
fn povey_window(length: usize) -> Vec<f32> {
    if length <= 1 {
        return vec![1.0; length];
    }

    let denominator = (length - 1) as f32;
    (0..length)
        .map(|i| {
            let hann = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / denominator).cos();
            hann.powf(0.85)
        })
        .collect()
}

fn hz_to_mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * ((mel / 1127.0).exp() - 1.0)
}

/// Triangular mel filters, equally spaced on the mel scale.
///
/// Each filter is stored as its first FFT bin plus dense weights from there, rather than a full
/// row of mostly zeros: at 80 filters over 257 bins the dense form is ~95% zeros and the
/// multiply dominates extraction time.
fn mel_filters(config: &FbankConfig, fft_size: usize) -> Vec<(usize, Vec<f32>)> {
    let num_bins = fft_size / 2 + 1;
    let bin_width = config.sample_rate as f32 / fft_size as f32;

    let low_mel = hz_to_mel(config.low_freq_hz);
    let high_mel = hz_to_mel(config.resolved_high_freq());
    // n filters need n+2 edges: each filter spans from its left neighbour's centre to its
    // right neighbour's centre.
    let mel_step = (high_mel - low_mel) / (config.num_mel_bins + 1) as f32;

    let mut filters = Vec::with_capacity(config.num_mel_bins);

    for f in 0..config.num_mel_bins {
        let left = mel_to_hz(low_mel + mel_step * f as f32);
        let centre = mel_to_hz(low_mel + mel_step * (f + 1) as f32);
        let right = mel_to_hz(low_mel + mel_step * (f + 2) as f32);

        let mut offset = None;
        let mut weights = Vec::new();

        for bin in 0..num_bins {
            let hz = bin as f32 * bin_width;
            let weight = if hz > left && hz < centre {
                (hz - left) / (centre - left)
            } else if hz >= centre && hz < right {
                (right - hz) / (right - centre)
            } else {
                0.0
            };

            if weight > 0.0 {
                if offset.is_none() {
                    offset = Some(bin);
                }
                weights.push(weight);
            } else if offset.is_some() {
                // Past the right edge: a triangle is contiguous, so this filter is done.
                break;
            }
        }

        filters.push((offset.unwrap_or(0), weights));
    }

    filters
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn tone(hz: f32, ms: usize) -> Vec<f32> {
        let n = RATE as usize * ms / 1000;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / RATE as f32).sin() * 0.5)
            .collect()
    }

    fn extractor() -> FbankExtractor {
        FbankExtractor::new(FbankConfig::default())
    }

    // --------------------------------------------------------------- shape

    #[test]
    fn the_default_config_matches_the_kaldi_convention() {
        let config = FbankConfig::default();
        assert_eq!(config.frame_length(), 400, "25 ms at 16 kHz");
        assert_eq!(config.frame_shift(), 160, "10 ms at 16 kHz");
        assert_eq!(config.fft_size(), 512, "next power of two above 400");
        assert_eq!(config.num_mel_bins, 80);
        assert_eq!(config.resolved_high_freq(), 7600.0, "nyquist - 400");
    }

    /// Frame count must follow `(len - frame_length) / shift + 1`. Off by one here shifts every
    /// downstream timestamp.
    #[test]
    fn the_frame_count_is_exact() {
        let fbank = extractor().compute(&tone(440.0, 1_000));
        // 16000 samples, 400-sample frames, 160-sample hop.
        assert_eq!(fbank.frames, (16_000 - 400) / 160 + 1);
        assert_eq!(fbank.num_bins, 80);
        assert_eq!(fbank.data.len(), fbank.frames * 80);
    }

    /// Audio shorter than one frame yields nothing rather than a zero-padded frame. A model
    /// asked to embed mostly-silence returns a confident vector for nothing.
    #[test]
    fn audio_shorter_than_one_frame_yields_no_frames() {
        let fbank = extractor().compute(&tone(440.0, 10));
        assert!(fbank.is_empty());
        assert_eq!(fbank.frames, 0);
        assert!(fbank.data.is_empty());
    }

    #[test]
    fn empty_input_is_handled() {
        let fbank = extractor().compute(&[]);
        assert_eq!(fbank.frames, 0);
        assert_eq!(fbank.num_bins, 80);
    }

    // --------------------------------------------------------------- correctness

    /// The load-bearing property: a tone must light up the mel bin covering its frequency.
    /// If this fails, the filterbank is wrong and every embedding built on it is noise.
    #[test]
    fn a_tone_peaks_in_the_mel_bin_containing_its_frequency() {
        let config = FbankConfig {
            mean_normalize: false,
            ..Default::default()
        };
        let extractor = FbankExtractor::new(config);

        for hz in [300.0f32, 1000.0, 3000.0] {
            let fbank = extractor.compute(&tone(hz, 500));
            let frame = fbank.frame(fbank.frames / 2);

            let peak = frame
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(i, _)| i)
                .expect("a peak");

            // Which filter should contain this frequency?
            let low_mel = hz_to_mel(config.low_freq_hz);
            let step = (hz_to_mel(config.resolved_high_freq()) - low_mel)
                / (config.num_mel_bins + 1) as f32;
            let expected = ((hz_to_mel(hz) - low_mel) / step - 1.0).round() as isize;

            assert!(
                (peak as isize - expected).abs() <= 1,
                "{hz} Hz peaked at filter {peak}, expected about {expected}"
            );
        }
    }

    /// Louder audio must produce larger features. With a natural log, doubling amplitude adds
    /// `2·ln(2) ≈ 1.386` to every bin — a base-10 log would add 0.602, and that constant factor
    /// is exactly the silent mismatch this test exists to catch.
    #[test]
    fn the_log_is_natural_not_base_ten() {
        let extractor = FbankExtractor::new(FbankConfig {
            mean_normalize: false,
            ..Default::default()
        });

        let quiet = extractor.compute(&tone(1000.0, 300));
        let loud_samples: Vec<f32> = tone(1000.0, 300).iter().map(|s| s * 2.0).collect();
        let loud = extractor.compute(&loud_samples);

        let mid = quiet.frames / 2;
        let peak_bin = quiet
            .frame(mid)
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();

        let delta = loud.frame(mid)[peak_bin] - quiet.frame(mid)[peak_bin];
        assert!(
            (delta - 2.0 * 2.0f32.ln()).abs() < 0.05,
            "doubling amplitude changed the feature by {delta}, expected {} (natural log)",
            2.0 * 2.0f32.ln()
        );
    }

    /// Povey is Hann^0.85, not Hamming. The shapes are close enough that a mismatch never
    /// looks obviously wrong — only slightly worse.
    #[test]
    fn the_window_is_povey_not_hamming() {
        let window = povey_window(400);

        assert_eq!(window.len(), 400);
        // Hann is zero at both ends; Hamming is 0.08.
        assert!(window[0] < 1e-6, "starts at {}", window[0]);
        assert!(window[399] < 1e-6, "ends at {}", window[399]);
        assert!((window[199] - 1.0).abs() < 0.01, "peak is {}", window[199]);

        // Off-peak, Povey sits above plain Hann because raising a value below 1 to 0.85
        // increases it.
        let i = 100usize;
        let hann = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / 399.0).cos();
        assert!(
            window[i] > hann,
            "{} should exceed hann {}",
            window[i],
            hann
        );
        assert!((window[i] - hann.powf(0.85)).abs() < 1e-6);
    }

    #[test]
    fn the_mel_scale_round_trips() {
        for hz in [0.0f32, 100.0, 700.0, 1000.0, 4000.0, 8000.0] {
            assert!(
                (mel_to_hz(hz_to_mel(hz)) - hz).abs() < 0.01,
                "{hz} Hz did not round-trip"
            );
        }
        // Monotonic, or filters would overlap in the wrong order.
        assert!(hz_to_mel(100.0) < hz_to_mel(1000.0));
        assert!(hz_to_mel(1000.0) < hz_to_mel(8000.0));
    }

    #[test]
    fn every_mel_filter_is_non_empty_and_within_range() {
        let config = FbankConfig::default();
        let filters = mel_filters(&config, config.fft_size());
        let num_bins = config.fft_size() / 2 + 1;

        assert_eq!(filters.len(), 80);
        for (i, (offset, weights)) in filters.iter().enumerate() {
            assert!(!weights.is_empty(), "filter {i} is empty");
            assert!(
                offset + weights.len() <= num_bins,
                "filter {i} runs past the spectrum"
            );
            for w in weights {
                assert!((0.0..=1.0).contains(w), "filter {i} weight {w}");
            }
        }
    }

    /// Filters must be ordered by frequency, or the feature vector is a permutation of what
    /// the model expects — which produces output without producing an error.
    #[test]
    fn mel_filters_are_ordered_by_frequency() {
        let config = FbankConfig::default();
        let filters = mel_filters(&config, config.fft_size());
        for pair in filters.windows(2) {
            assert!(
                pair[1].0 >= pair[0].0,
                "filters out of order: {} then {}",
                pair[0].0,
                pair[1].0
            );
        }
    }

    /// Mean normalisation removes the constant channel component. Without it, two recordings of
    /// the same voice on different microphones look like different speakers.
    #[test]
    fn mean_normalization_centres_each_bin_on_zero() {
        let fbank = extractor().compute(&tone(1000.0, 500));

        for bin in 0..fbank.num_bins {
            let mean: f32 = (0..fbank.frames)
                .map(|f| fbank.data[f * fbank.num_bins + bin])
                .sum::<f32>()
                / fbank.frames as f32;
            assert!(mean.abs() < 1e-3, "bin {bin} has mean {mean}");
        }
    }

    #[test]
    fn features_are_finite_even_for_digital_silence() {
        // log(0) is -inf; the floor is what keeps it out of the model.
        let fbank = extractor().compute(&vec![0.0; 16_000]);
        assert!(fbank.frames > 0);
        for (i, value) in fbank.data.iter().enumerate() {
            assert!(value.is_finite(), "feature {i} is {value}");
        }
    }

    /// Extraction must be deterministic: the same audio has to embed identically, or the same
    /// speaker drifts between clusters.
    #[test]
    fn extraction_is_deterministic() {
        let extractor = extractor();
        let audio = tone(440.0, 300);
        assert_eq!(extractor.compute(&audio), extractor.compute(&audio));
    }

    /// Pre-emphasis must use the original previous sample, not the filtered one. Applying it
    /// left to right compounds the filter and tilts the spectrum far more than intended.
    #[test]
    fn preemphasis_boosts_high_frequencies_relative_to_low() {
        let plain = FbankExtractor::new(FbankConfig {
            preemphasis: 0.0,
            mean_normalize: false,
            ..Default::default()
        });
        let emphasised = FbankExtractor::new(FbankConfig {
            mean_normalize: false,
            ..Default::default()
        });

        let low = tone(200.0, 300);
        let high = tone(4000.0, 300);

        let gain = |e: &FbankExtractor, audio: &[f32]| -> f32 {
            let fbank = e.compute(audio);
            *fbank
                .frame(fbank.frames / 2)
                .iter()
                .max_by(|a, b| a.partial_cmp(b).unwrap())
                .unwrap()
        };

        let low_change = gain(&emphasised, &low) - gain(&plain, &low);
        let high_change = gain(&emphasised, &high) - gain(&plain, &high);

        assert!(
            high_change > low_change,
            "pre-emphasis should favour high frequencies: low {low_change:.2}, high {high_change:.2}"
        );
    }
}
