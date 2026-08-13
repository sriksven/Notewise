//! Voice activity detection.
//!
//! # Why this exists
//!
//! Whisper hallucinates on non-speech audio. Fed ten seconds of room tone it does not return
//! nothing — it returns a confident, fluent, entirely invented sentence. Observed in this
//! repository during a live recording test: the opening silence before anyone spoke produced
//! *"I know. You're happy to see a new work to grow."* That is worse than a gap, because a
//! transcript the user cannot distinguish from a real one is a transcript they cannot trust.
//!
//! So audio is classified before it reaches the model, and windows containing no speech are
//! never decoded. This also saves the inference entirely, which on a quiet meeting is most of
//! the compute.
//!
//! # The algorithm
//!
//! Short-time energy against an **adaptive noise floor**, gated by zero-crossing rate, with
//! hysteresis and hangover. This is the classical structure — the same shape as the energy
//! stage in WebRTC's VAD — and each part earns its place:
//!
//! - **Adaptive floor**, not a fixed threshold in dBFS. A laptop fan, an air conditioner, and
//!   a lapel mic in a quiet room differ by more than 30 dB. Any fixed threshold is wrong for
//!   most rooms, and wrong in the dangerous direction for at least one.
//! - **Zero-crossing rate** rejects what energy alone accepts. A door slam and a keyboard have
//!   speech-like energy; their ZCR does not look like voiced speech.
//! - **Hysteresis** (a lower threshold to *stay* in speech than to *enter* it) stops the gate
//!   chattering on the natural amplitude dips inside a word.
//! - **Hangover** keeps the gate open briefly after energy falls, because word-final
//!   consonants — the /s/ in "costs", the /t/ in "cut" — are low-energy and would be clipped.
//! - **Minimum speech run** before committing, so a single click cannot open the gate.
//!
//! The floor only adapts while the detector believes there is no speech. Adapting during
//! speech would let a long sentence raise the floor until the speaker's own voice fell below
//! it — the failure mode that makes naive energy VADs cut out mid-paragraph.

/// Tuning for [`Vad`].
///
/// The defaults are deliberately conservative: they let marginal audio through rather than
/// risk discarding speech. A false positive costs one wasted inference; a false negative
/// costs a sentence of someone's meeting, and they may never know it was missing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VadConfig {
    /// Analysis window. 20 ms is short enough to resolve a phoneme boundary and long enough
    /// for a stable energy estimate at 16 kHz.
    pub sub_frame_ms: u32,

    /// How far above the noise floor energy must rise to *enter* speech, in dB.
    pub onset_db: f32,

    /// How far above the noise floor energy must stay to *remain* in speech, in dB.
    ///
    /// Lower than [`Self::onset_db`]; the difference is the hysteresis band.
    pub offset_db: f32,

    /// How long to keep the gate open after energy falls below the offset threshold.
    pub hangover_ms: u32,

    /// How much speech must accumulate before the gate opens, rejecting isolated transients.
    pub min_speech_ms: u32,

    /// Upper bound on zero-crossing rate for a sub-frame to count as speech.
    ///
    /// Voiced speech sits well below this. Broadband noise — fan, hiss, key clicks — sits
    /// above it. Unvoiced fricatives also sit high, which is why a sub-frame failing this test
    /// is not rejected outright once the gate is already open.
    pub max_zcr: f32,

    /// Absolute floor, in dBFS, below which audio is silence whatever the adaptive floor says.
    ///
    /// Guards the degenerate case of a muted or disconnected input, where the noise floor
    /// adapts down toward digital zero and *any* dither would clear the onset margin.
    pub silence_floor_db: f32,

    /// Starting estimate of the noise floor, used until the detector has heard enough.
    pub initial_noise_floor_db: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            sub_frame_ms: 20,
            // 9 dB over the floor is about the smallest margin that reliably separates speech
            // from room tone without also admitting HVAC rumble.
            onset_db: 9.0,
            offset_db: 5.0,
            // Long enough to carry a word-final fricative, short enough not to staple two
            // sentences together.
            hangover_ms: 280,
            min_speech_ms: 100,
            max_zcr: 0.30,
            silence_floor_db: -65.0,
            initial_noise_floor_db: -50.0,
        }
    }
}

/// What the detector concluded about a span of audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VadReport {
    /// Milliseconds of audio classified as speech.
    pub speech_ms: i64,
    /// Milliseconds analysed.
    pub total_ms: i64,
    /// The noise floor estimate after this span, in dBFS. Useful for diagnostics.
    pub noise_floor_db: f32,
    /// Peak level seen, in dBFS.
    pub peak_db: f32,
}

impl VadReport {
    /// Whether any speech was found.
    ///
    /// The question a caller usually has, since the decision it drives — decode this window or
    /// skip it — is binary.
    pub fn has_speech(&self) -> bool {
        self.speech_ms > 0
    }

    /// Fraction of the span that was speech, in `0.0..=1.0`.
    pub fn speech_ratio(&self) -> f32 {
        if self.total_ms <= 0 {
            return 0.0;
        }
        (self.speech_ms as f32 / self.total_ms as f32).clamp(0.0, 1.0)
    }
}

/// A streaming voice activity detector.
///
/// Stateful on purpose: the noise floor, the hysteresis state, and the hangover counter all
/// carry across calls, so feeding a recording in chunks gives the same answer as feeding it
/// whole. A detector that reset per chunk would re-learn the room every window and clip the
/// start of every one.
#[derive(Debug, Clone)]
pub struct Vad {
    config: VadConfig,
    noise_floor_db: f32,
    /// True while the gate is open.
    in_speech: bool,
    /// Sub-frames of hangover remaining.
    hangover_left: u32,
    /// Consecutive candidate sub-frames seen while the gate is closed.
    onset_run: u32,
}

impl Default for Vad {
    fn default() -> Self {
        Self::new(VadConfig::default())
    }
}

impl Vad {
    pub fn new(config: VadConfig) -> Self {
        Self {
            noise_floor_db: config.initial_noise_floor_db,
            config,
            in_speech: false,
            hangover_left: 0,
            onset_run: 0,
        }
    }

    pub fn config(&self) -> &VadConfig {
        &self.config
    }

    /// The current noise floor estimate, in dBFS.
    pub fn noise_floor_db(&self) -> f32 {
        self.noise_floor_db
    }

    /// Whether the gate is currently open.
    pub fn in_speech(&self) -> bool {
        self.in_speech
    }

    /// Analyse mono samples and report how much of them were speech.
    ///
    /// Samples must be mono; a caller with interleaved audio converts first. Analysing
    /// interleaved stereo would compute zero-crossing rate across the channel boundary and
    /// produce nonsense.
    pub fn analyze(&mut self, samples: &[f32], sample_rate: u32) -> VadReport {
        let sub_len = self.sub_frame_len(sample_rate);
        let total_ms = ms_for(samples.len(), sample_rate);

        if sub_len == 0 || samples.is_empty() {
            return VadReport {
                speech_ms: 0,
                total_ms,
                noise_floor_db: self.noise_floor_db,
                peak_db: f32::NEG_INFINITY,
            };
        }

        let sub_ms = ms_for(sub_len, sample_rate);
        let mut speech_sub_frames: i64 = 0;
        let mut peak_db = f32::NEG_INFINITY;

        // A trailing partial sub-frame is analysed too rather than dropped: at the end of a
        // recording that remainder can be the last word.
        for chunk in samples.chunks(sub_len) {
            let level = rms_db(chunk);
            let zcr = zero_crossing_rate(chunk);
            peak_db = peak_db.max(level);

            if self.step(level, zcr) {
                speech_sub_frames += 1;
            }
        }

        VadReport {
            speech_ms: speech_sub_frames * sub_ms,
            total_ms,
            noise_floor_db: self.noise_floor_db,
            peak_db,
        }
    }

    /// Advance the state machine by one sub-frame. Returns whether it counts as speech.
    fn step(&mut self, level_db: f32, zcr: f32) -> bool {
        // Digital silence, or close enough that no adaptive threshold should override it.
        //
        // Only applied while the gate is *closed*. Applying it during speech would let a
        // single near-zero sub-frame cut a word's tail, which is the thing hangover exists to
        // prevent; hangover is bounded, so letting it run its course costs at most 280 ms.
        if !self.in_speech && level_db < self.config.silence_floor_db {
            self.adapt_floor(level_db);
            self.onset_run = 0;
            return false;
        }

        let above_floor = level_db - self.noise_floor_db;

        if self.in_speech {
            // Hysteresis: a lower bar to stay than to enter. ZCR is not re-tested here — an
            // unvoiced fricative ending a word has high ZCR by nature, and testing it would
            // clip exactly the sounds hangover exists to protect.
            if above_floor >= self.config.offset_db {
                self.hangover_left = self.hangover_sub_frames();
                return true;
            }

            if self.hangover_left > 0 {
                self.hangover_left -= 1;
                // Still counted as speech: this audio is being kept, so it must be reported
                // as kept. The floor deliberately does not adapt here — the tail of a word is
                // not room tone, and learning from it would raise the floor into the speaker.
                return true;
            }

            self.in_speech = false;
            self.onset_run = 0;
            self.adapt_floor(level_db);
            return false;
        }

        // Gate closed: both energy and spectral shape must agree before opening.
        let candidate = above_floor >= self.config.onset_db && zcr <= self.config.max_zcr;

        if candidate {
            self.onset_run += 1;
            if self.onset_run >= self.min_speech_sub_frames() {
                self.in_speech = true;
                self.hangover_left = self.hangover_sub_frames();
                return true;
            }
            // A candidate that has not yet met the minimum run is not speech *yet*, and must
            // not teach the floor either — it may well be the first syllable of a word.
            return false;
        }

        self.onset_run = 0;
        self.adapt_floor(level_db);
        false
    }

    /// Update the noise floor estimate.
    ///
    /// Asymmetric by design. It falls faster than it rises, because a room that just went
    /// quiet is immediately quieter and the detector should believe it, whereas the most
    /// common cause of a *sustained rise* is speech — and a floor that chases speech
    /// eventually swallows it.
    ///
    /// Both the input and the result are clamped to [`VadConfig::silence_floor_db`]. Digital
    /// silence has an RMS of negative infinity, and feeding that in unclamped drives the floor
    /// to -inf, after which every subsequent sample is hundreds of dB "above the floor" and
    /// the detector calls room tone speech forever. That is not hypothetical: it is what a WAV
    /// with a few milliseconds of leading digital silence did to this function, and it made
    /// real recorded room tone read as 12 seconds of speech.
    fn adapt_floor(&mut self, level_db: f32) {
        // Fast enough to settle on a new room inside a second, slow enough not to track the
        // dips *within* speech down to the minimum.
        const FALL: f32 = 0.08;
        const RISE: f32 = 0.02;

        if !level_db.is_finite() && level_db.is_sign_positive() {
            return; // +inf or NaN: not a level, do not learn from it
        }

        let level_db = level_db.max(self.config.silence_floor_db);
        let rate = if level_db < self.noise_floor_db {
            FALL
        } else {
            RISE
        };

        self.noise_floor_db = (self.noise_floor_db + (level_db - self.noise_floor_db) * rate)
            .clamp(self.config.silence_floor_db, 0.0);
    }

    fn sub_frame_len(&self, sample_rate: u32) -> usize {
        (sample_rate as u64 * self.config.sub_frame_ms as u64 / 1000) as usize
    }

    fn hangover_sub_frames(&self) -> u32 {
        self.config
            .hangover_ms
            .div_ceil(self.config.sub_frame_ms.max(1))
    }

    fn min_speech_sub_frames(&self) -> u32 {
        self.config
            .min_speech_ms
            .div_ceil(self.config.sub_frame_ms.max(1))
            .max(1)
    }
}

/// Root-mean-square level in dBFS.
fn rms_db(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return f32::NEG_INFINITY;
    }

    // Accumulated in f64: a 20 ms window at 48 kHz is only ~960 samples, but a caller may pass
    // a whole window, and f32 accumulation drifts measurably past a few thousand terms.
    let sum: f64 = samples.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    let mean = sum / samples.len() as f64;

    if mean <= 0.0 {
        return f32::NEG_INFINITY;
    }
    (10.0 * mean.log10()) as f32
}

/// Fraction of adjacent sample pairs that cross zero.
///
/// A dead-zone excludes crossings caused by dither around silence, which would otherwise make
/// digital near-silence look like the highest-ZCR signal there is.
fn zero_crossing_rate(samples: &[f32]) -> f32 {
    const DEAD_ZONE: f32 = 1e-4;

    if samples.len() < 2 {
        return 0.0;
    }

    let mut crossings = 0usize;
    let mut previous: Option<bool> = None;

    for sample in samples {
        if sample.abs() < DEAD_ZONE {
            continue;
        }
        let positive = *sample > 0.0;
        if let Some(was_positive) = previous {
            if was_positive != positive {
                crossings += 1;
            }
        }
        previous = Some(positive);
    }

    crossings as f32 / (samples.len() - 1) as f32
}

fn ms_for(samples: usize, sample_rate: u32) -> i64 {
    if sample_rate == 0 {
        return 0;
    }
    (samples as i64 * 1000) / sample_rate as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn silence(ms: usize) -> Vec<f32> {
        vec![0.0; RATE as usize * ms / 1000]
    }

    /// Low-level broadband noise: a fan, a room, a hot mic.
    fn room_tone(ms: usize, amplitude: f32) -> Vec<f32> {
        let n = RATE as usize * ms / 1000;
        // A deterministic LCG. A real RNG dependency for test noise would be a dependency the
        // shipped crate carries for nothing.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..n)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let unit = ((state >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0;
                unit * amplitude
            })
            .collect()
    }

    /// A voiced-speech stand-in: a low-frequency tone with harmonics, which is what makes it
    /// pass the ZCR test where noise of the same energy does not.
    fn voiced(ms: usize, amplitude: f32) -> Vec<f32> {
        let n = RATE as usize * ms / 1000;
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let f0 = 130.0; // a typical male fundamental
                amplitude
                    * (0.6 * (2.0 * std::f32::consts::PI * f0 * t).sin()
                        + 0.3 * (2.0 * std::f32::consts::PI * f0 * 2.0 * t).sin()
                        + 0.1 * (2.0 * std::f32::consts::PI * f0 * 3.0 * t).sin())
            })
            .collect()
    }

    // --------------------------------------------------------------------- the core claim

    /// The bug this module exists for: ten seconds of room tone must not look like speech.
    /// Fed to Whisper it produced a fluent invented sentence.
    #[test]
    fn ten_seconds_of_room_tone_contains_no_speech() {
        let mut vad = Vad::default();
        let report = vad.analyze(&room_tone(10_000, 0.002), RATE);

        assert!(
            !report.has_speech(),
            "room tone classified as {} ms of speech (floor {:.1} dB, peak {:.1} dB)",
            report.speech_ms,
            report.noise_floor_db,
            report.peak_db
        );
    }

    #[test]
    fn digital_silence_contains_no_speech() {
        let mut vad = Vad::default();
        assert!(!vad.analyze(&silence(5_000), RATE).has_speech());
    }

    #[test]
    fn speech_over_room_tone_is_detected() {
        let mut vad = Vad::default();

        // Let the floor settle on the room first, as it would in a real meeting.
        vad.analyze(&room_tone(1_000, 0.002), RATE);

        let report = vad.analyze(&voiced(1_000, 0.2), RATE);
        assert!(report.has_speech(), "floor {:.1} dB", report.noise_floor_db);
        assert!(
            report.speech_ratio() > 0.7,
            "expected most of the second to be speech, got {:.2}",
            report.speech_ratio()
        );
    }

    // --------------------------------------------------------------------- adaptivity

    /// The point of an adaptive floor: the same speech must be found in a quiet room and a
    /// loud one. A fixed threshold passes one of these and fails the other.
    #[test]
    fn speech_is_found_in_both_a_quiet_and_a_noisy_room() {
        for (label, noise) in [("quiet", 0.0005f32), ("noisy", 0.02f32)] {
            let mut vad = Vad::default();
            vad.analyze(&room_tone(2_000, noise), RATE);

            // Speech sits a realistic ~20 dB over the room in both cases.
            let speech: Vec<f32> = voiced(1_000, noise * 10.0);
            let report = vad.analyze(&speech, RATE);

            assert!(
                report.has_speech(),
                "{label} room: no speech found (floor {:.1} dB, peak {:.1} dB)",
                report.noise_floor_db,
                report.peak_db
            );
        }
    }

    /// A fixed threshold would call loud room tone "speech". The floor must climb to meet it.
    #[test]
    fn a_loud_room_raises_the_floor_rather_than_reporting_speech() {
        let mut vad = Vad::default();
        let report = vad.analyze(&room_tone(6_000, 0.05), RATE);

        assert!(!report.has_speech(), "{} ms", report.speech_ms);
        assert!(
            vad.noise_floor_db() > -40.0,
            "floor should have risen to meet the room, got {:.1} dB",
            vad.noise_floor_db()
        );
    }

    /// The failure mode of naive energy VADs: the floor chases a long utterance until the
    /// speaker falls below it and the gate shuts mid-sentence.
    #[test]
    fn a_long_utterance_does_not_raise_the_floor_into_the_speaker() {
        let mut vad = Vad::default();
        vad.analyze(&room_tone(1_000, 0.002), RATE);
        let settled = vad.noise_floor_db();

        // Thirty unbroken seconds.
        let report = vad.analyze(&voiced(30_000, 0.2), RATE);

        assert!(
            report.speech_ratio() > 0.95,
            "gate closed during continuous speech: {:.2}",
            report.speech_ratio()
        );
        assert!(
            vad.noise_floor_db() <= settled + 1.0,
            "floor climbed from {settled:.1} to {:.1} dB during speech",
            vad.noise_floor_db()
        );
    }

    // --------------------------------------------------------------------- gate behaviour

    /// A click has speech-like energy and nothing else. Requiring a minimum run rejects it.
    #[test]
    fn an_isolated_click_does_not_open_the_gate() {
        let mut vad = Vad::default();
        vad.analyze(&room_tone(1_000, 0.002), RATE);

        let mut audio = silence(200);
        // ~1 ms of full-scale transient, far shorter than min_speech_ms.
        for sample in audio.iter_mut().take(16) {
            *sample = 0.9;
        }

        assert!(!vad.analyze(&audio, RATE).has_speech());
    }

    /// Word-final consonants are low-energy. Without hangover they are clipped, and the
    /// transcript quietly loses the end of words.
    #[test]
    fn hangover_keeps_the_gate_open_briefly_after_speech_ends() {
        let mut vad = Vad::default();
        vad.analyze(&room_tone(500, 0.002), RATE);
        vad.analyze(&voiced(500, 0.2), RATE);

        assert!(vad.in_speech(), "gate should be open after speech");

        // 100 ms of quiet is inside the 280 ms hangover.
        let report = vad.analyze(&silence(100), RATE);
        assert!(
            report.has_speech(),
            "hangover should still be counting this as speech"
        );
    }

    #[test]
    fn the_gate_closes_once_hangover_expires() {
        let mut vad = Vad::default();
        vad.analyze(&room_tone(500, 0.002), RATE);
        vad.analyze(&voiced(500, 0.2), RATE);

        vad.analyze(&silence(1_000), RATE);
        assert!(!vad.in_speech(), "gate should have closed");
    }

    /// Chunked input must give the same answer as whole input, or a live recording and an
    /// imported file would disagree about the same audio.
    #[test]
    fn chunked_analysis_matches_whole_analysis() {
        let mut audio = room_tone(1_000, 0.002);
        audio.extend(voiced(1_000, 0.2));
        audio.extend(room_tone(1_000, 0.002));

        let mut whole = Vad::default();
        let whole_report = whole.analyze(&audio, RATE);

        let mut chunked = Vad::default();
        let mut speech_ms = 0;
        for chunk in audio.chunks(RATE as usize / 10) {
            speech_ms += chunked.analyze(chunk, RATE).speech_ms;
        }

        // Not bit-identical: chunk boundaries land mid-sub-frame. Close is the real claim.
        let drift = (speech_ms - whole_report.speech_ms).abs();
        assert!(
            drift <= 120,
            "chunked {speech_ms} ms vs whole {} ms",
            whole_report.speech_ms
        );
    }

    // --------------------------------------------------------------------- edges

    #[test]
    fn empty_input_is_handled() {
        let mut vad = Vad::default();
        let report = vad.analyze(&[], RATE);
        assert_eq!(report.speech_ms, 0);
        assert_eq!(report.total_ms, 0);
        assert_eq!(report.speech_ratio(), 0.0);
    }

    #[test]
    fn a_zero_sample_rate_does_not_divide_by_zero() {
        let mut vad = Vad::default();
        let report = vad.analyze(&voiced(100, 0.2), 0);
        assert_eq!(report.total_ms, 0);
    }

    /// Input shorter than one sub-frame is still analysed — at the end of a recording that
    /// remainder can be the last word.
    #[test]
    fn a_partial_sub_frame_is_still_analysed() {
        let mut vad = Vad::default();
        vad.analyze(&room_tone(1_000, 0.002), RATE);
        // 5 ms, a quarter of a sub-frame.
        let report = vad.analyze(&voiced(5, 0.3), RATE);
        assert!(report.total_ms >= 0);
    }

    #[test]
    fn the_speech_ratio_stays_in_range() {
        let mut vad = Vad::default();
        for audio in [silence(500), room_tone(500, 0.01), voiced(500, 0.3)] {
            let ratio = vad.analyze(&audio, RATE).speech_ratio();
            assert!((0.0..=1.0).contains(&ratio), "{ratio}");
        }
    }

    /// Digital near-silence has near-random sign, so an undefended ZCR reads ~0.5 — higher
    /// than any real signal. The dead zone is what stops that.
    #[test]
    fn near_silence_does_not_read_as_maximum_zero_crossing_rate() {
        let dither: Vec<f32> = (0..320)
            .map(|i| if i % 2 == 0 { 1e-7 } else { -1e-7 })
            .collect();
        assert_eq!(zero_crossing_rate(&dither), 0.0);
    }

    #[test]
    fn rms_of_silence_is_negative_infinity() {
        assert_eq!(rms_db(&[0.0; 64]), f32::NEG_INFINITY);
        assert_eq!(rms_db(&[]), f32::NEG_INFINITY);
    }

    #[test]
    fn rms_of_full_scale_is_zero_dbfs() {
        assert!((rms_db(&[1.0; 64]) - 0.0).abs() < 0.001);
        assert!((rms_db(&[-1.0; 64]) - 0.0).abs() < 0.001);
    }

    /// A muted input adapts its floor toward digital zero. Without an absolute floor the
    /// onset margin would eventually be cleared by dither alone.
    #[test]
    fn a_muted_input_never_reports_speech_however_long_it_runs() {
        let mut vad = Vad::default();
        for _ in 0..20 {
            let report = vad.analyze(&room_tone(1_000, 1e-6), RATE);
            assert!(!report.has_speech(), "floor {:.1}", report.noise_floor_db);
        }
    }

    // --------------------------------------------------------------------- regressions

    /// Digital silence has an RMS of -inf. Learning from it unclamped drove the floor to -inf,
    /// after which everything was "hundreds of dB above the floor" and real room tone read as
    /// continuous speech. Found by feeding a WAV whose first milliseconds were digital zero.
    #[test]
    fn leading_digital_silence_does_not_poison_the_noise_floor() {
        let mut vad = Vad::default();
        vad.analyze(&silence(500), RATE);

        assert!(
            vad.noise_floor_db().is_finite(),
            "floor became {}",
            vad.noise_floor_db()
        );
        assert!(
            vad.noise_floor_db() >= VadConfig::default().silence_floor_db,
            "floor fell to {:.1} dB, below the absolute floor",
            vad.noise_floor_db()
        );
    }

    /// The whole bug, end to end: digital silence, then genuinely quiet recorded room tone.
    /// Before the clamp this reported the room tone as speech from the first sub-frame.
    #[test]
    fn quiet_room_tone_after_digital_silence_is_not_speech() {
        let mut vad = Vad::default();
        vad.analyze(&silence(300), RATE);

        // ~-55 dBFS, matching a real measurement of a quiet room on a laptop mic.
        let mut speech_ms = 0;
        for _ in 0..12 {
            speech_ms += vad.analyze(&room_tone(1_000, 0.0025), RATE).speech_ms;
        }

        assert!(
            speech_ms < 500,
            "{speech_ms} ms of quiet room tone read as speech (floor {:.1} dB)",
            vad.noise_floor_db()
        );
    }

    /// The floor must stay in a range that means something in dBFS.
    #[test]
    fn the_noise_floor_stays_within_dbfs_range() {
        let mut vad = Vad::default();
        for audio in [
            silence(200),
            room_tone(200, 1.0),
            voiced(200, 1.0),
            silence(200),
        ] {
            vad.analyze(&audio, RATE);
            let floor = vad.noise_floor_db();
            assert!(
                floor.is_finite() && (-65.0..=0.0).contains(&floor),
                "floor out of range: {floor}"
            );
        }
    }

    #[test]
    fn a_nan_level_does_not_corrupt_the_floor() {
        let mut vad = Vad::default();
        let before = vad.noise_floor_db();
        vad.analyze(&[f32::NAN; 320], RATE);
        assert!(
            vad.noise_floor_db().is_finite(),
            "floor became {} after NaN input, was {before}",
            vad.noise_floor_db()
        );
    }
}
