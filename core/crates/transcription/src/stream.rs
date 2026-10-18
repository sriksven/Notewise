//! Grouping streaming audio into utterances.
//!
//! # Why this is not a fixed window
//!
//! A decoder transcribes a buffer, not a stream, so live audio has to be cut into buffers
//! somewhere. Cutting on a fixed clock — the obvious approach, and what this crate did first —
//! goes wrong in three ways at once, all of which were observed in one 15-second recording:
//!
//! 1. **Nothing appears until the first window closes.** With a ten-second window the
//!    transcript is empty for ten seconds. A live transcript that lags the room by that much
//!    is not a live transcript.
//!
//! 2. **Silence gets decoded, and the model invents.** Whisper fed room tone does not return
//!    nothing; it returns fluent, confident text. Five seconds of a quiet room at the end of
//!    that recording became `Okay.` four times over.
//!
//! 3. **Segment timings drift into fiction.** Advancing the clock by the whole window, when
//!    speech occupied only its first two seconds, leaves an eight-second hole between one
//!    segment and the next. Downstream, anything reading gaps — diarization especially —
//!    reads that hole as a real pause.
//!
//! Cutting where the *speaker* stops fixes all three. The gate is [`Vad`], which already
//! tracks an adaptive noise floor with hysteresis and hangover, so "has the speaker paused"
//! is a question this crate can already answer without a model.
//!
//! # What this guarantees
//!
//! - A buffer handed out always holds at least [`UtteranceConfig::min_speech_ms`] of speech.
//!   Silence is dropped, never decoded.
//! - `offset_ms` counts audio actually consumed, so a segment's timing reflects when it was
//!   said rather than which window it landed in.
//! - Unbroken speech still produces text every [`UtteranceConfig::max_utterance_ms`], so a
//!   monologue streams instead of going quiet.

use std::collections::VecDeque;

use notewise_audio_capture::Vad;

/// A buffer of audio worth decoding, and where it sits in the recording.
#[derive(Debug, Clone, PartialEq)]
pub struct Utterance {
    /// Mono samples at the transcription sample rate.
    pub samples: Vec<f32>,
    /// Milliseconds of audio that preceded these samples, for offsetting segment timings.
    pub offset_ms: i64,
}

impl Utterance {
    pub fn duration_ms(&self, sample_rate: u32) -> i64 {
        ms_for(self.samples.len(), sample_rate)
    }
}

/// Tuning for [`UtteranceBuffer`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UtteranceConfig {
    /// Trailing silence that marks the end of an utterance.
    ///
    /// Counted from when the gate *closes*, not from when energy drops — so [`Vad`]'s hangover
    /// (280 ms by default, protecting word-final consonants) is added to this before a phrase
    /// is handed over. The delay a user actually sees is therefore hangover + this + decode,
    /// which at the default lands a little under a second after the speaker stops.
    pub endpoint_silence_ms: i64,

    /// Decode even mid-phrase once a buffer reaches this length.
    ///
    /// Someone reading a list aloud can go a long time without a gap the gate notices. Without
    /// a cap the transcript would show nothing for as long as they kept going. The cost is a
    /// cut mid-phrase — which is why the cut is not made at the cap itself but at the quietest
    /// moment in the [`Self::cut_search_ms`] before it. See [`UtteranceBuffer::push`].
    pub max_utterance_ms: i64,

    /// How far back from the length cap to look for somewhere quiet to cut.
    ///
    /// A cut through a word loses it from both halves: the decoder drops the fragment at the end
    /// of one buffer and the fragment at the start of the next. Measured on eight clauses of
    /// unbroken speech, cutting exactly at the cap dropped "so the margin story" outright and
    /// turned "billing" into "big building". Even fluent speech dips between words and clauses
    /// every second or so, so two seconds of search finds one; what follows the cut is kept and
    /// begins the next buffer.
    pub cut_search_ms: i64,

    /// Speech a buffer must contain before it is worth decoding.
    ///
    /// The guard against the invented-text failure. A buffer that is mostly silence with one
    /// blip in it — a chair creak, a keyboard, a breath the gate let through — is exactly what
    /// makes the model hallucinate, and is never worth an inference.
    pub min_speech_ms: i64,

    /// Silence kept ahead of speech when discarding.
    ///
    /// The gate needs energy to *rise* before it opens, so the first few milliseconds of a
    /// softly-started word sit below the threshold. Discarding right up to the gate's decision
    /// point would clip them.
    pub preroll_ms: i64,

    /// Silence allowed to accumulate before it is discarded.
    ///
    /// Bounds memory in a quiet room, and keeps the leading edge of a buffer close to the
    /// speech in it so timings stay honest.
    pub max_silence_ms: i64,
}

impl Default for UtteranceConfig {
    fn default() -> Self {
        Self {
            // Vad's hangover is 280 ms; this must clear it.
            endpoint_silence_ms: 500,
            max_utterance_ms: 8_000,
            cut_search_ms: 2_000,
            min_speech_ms: 240,
            preroll_ms: 300,
            max_silence_ms: 1_000,
        }
    }
}

/// Cuts streaming audio into utterance-sized buffers.
///
/// Stateful across calls: the noise floor and the gate's hysteresis live in [`Vad`], and the
/// consumed-audio clock lives here. Feeding a recording in frames therefore gives the same
/// answer as feeding it whole.
#[derive(Debug)]
pub struct UtteranceBuffer {
    config: UtteranceConfig,
    vad: Vad,
    sample_rate: u32,

    /// Audio accumulated since the last decode or discard.
    buffer: Vec<f32>,
    /// What each pushed chunk still in `buffer` contributed, as `(samples, speech_ms)`, oldest
    /// first. Lets a cut part-way through the buffer say how much speech is left after it.
    chunks: VecDeque<(usize, i64)>,
    /// Samples consumed before `buffer` begins.
    ///
    /// Counted in samples rather than milliseconds: cuts and discards land on arbitrary samples,
    /// and rounding each one to a millisecond would drift a long meeting's timings by up to a
    /// millisecond per cut.
    offset_samples: u64,
    /// Speech found in `buffer`, in milliseconds.
    speech_ms: i64,
    /// Unbroken silence at the tail of `buffer`, in milliseconds.
    silence_ms: i64,

    /// Whether to gate on speech at all. Off is a diagnostic — see
    /// [`UtteranceBuffer::without_speech_gate`].
    gate_on_speech: bool,
}

impl UtteranceBuffer {
    pub fn new(sample_rate: u32) -> Self {
        Self::with_config(sample_rate, UtteranceConfig::default())
    }

    pub fn with_config(sample_rate: u32, config: UtteranceConfig) -> Self {
        Self {
            config,
            vad: Vad::default(),
            sample_rate,
            buffer: Vec::new(),
            chunks: VecDeque::new(),
            offset_samples: 0,
            speech_ms: 0,
            silence_ms: 0,
            gate_on_speech: true,
        }
    }

    /// Replace the speech gate.
    pub fn set_vad(&mut self, vad: Vad) {
        self.vad = vad;
    }

    /// Hand out every buffer, including ones with no speech in them.
    ///
    /// Turning this on brings the invented text back, and it exists for diagnosing the gate:
    /// if a transcript is missing words, running once ungated answers whether the gate or the
    /// model dropped them. With the gate off, buffers are cut on the length cap alone.
    pub fn disable_speech_gate(&mut self) {
        self.gate_on_speech = false;
    }

    pub fn config(&self) -> &UtteranceConfig {
        &self.config
    }

    /// Milliseconds of audio consumed before the current buffer.
    pub fn offset_ms(&self) -> i64 {
        if self.sample_rate == 0 {
            return 0;
        }
        (self.offset_samples * 1000 / self.sample_rate as u64) as i64
    }

    /// Audio currently held, in milliseconds.
    pub fn buffered_ms(&self) -> i64 {
        ms_for(self.buffer.len(), self.sample_rate)
    }

    /// Speech currently held, in milliseconds.
    pub fn speech_ms(&self) -> i64 {
        self.speech_ms
    }

    /// The noise floor the gate has settled on, in dBFS.
    pub fn noise_floor_db(&self) -> f32 {
        self.vad.noise_floor_db()
    }

    /// Add samples, returning a buffer to decode if one is ready.
    ///
    /// Samples must be mono at this buffer's sample rate.
    pub fn push(&mut self, samples: &[f32]) -> Option<Utterance> {
        if samples.is_empty() {
            return None;
        }

        let report = self.vad.analyze(samples, self.sample_rate);
        let duration_ms = ms_for(samples.len(), self.sample_rate);

        self.buffer.extend_from_slice(samples);
        self.chunks.push_back((samples.len(), report.speech_ms));
        self.speech_ms += report.speech_ms;
        if report.has_speech() {
            self.silence_ms = 0;
        } else {
            self.silence_ms += duration_ms;
        }

        if !self.gate_on_speech {
            // Diagnostic mode: length is the only rule.
            return (self.buffered_ms() >= self.config.max_utterance_ms).then(|| self.take());
        }

        let has_enough_speech = self.speech_ms >= self.config.min_speech_ms;

        // The speaker paused. This is the good cut: a whole phrase, ending where they stopped.
        if has_enough_speech && self.silence_ms >= self.config.endpoint_silence_ms {
            return Some(self.take());
        }

        if self.buffered_ms() >= self.config.max_utterance_ms {
            if has_enough_speech {
                // Still talking. Cut anyway so the transcript keeps moving — but where the speaker
                // is quietest, not wherever the cap happened to land, and keep what follows.
                let cut = self.quietest_cut();
                return Some(self.take_until(cut));
            }
            // Long, and nothing in it. Never send this to a model.
            self.discard();
            return None;
        }

        // A quiet room. Drop it rather than let it grow, keeping a little pre-roll in case the
        // next thing that happens is someone starting to talk.
        if !has_enough_speech && self.silence_ms >= self.config.max_silence_ms {
            self.discard();
        }

        None
    }

    /// Hand out whatever is buffered, if it is worth decoding.
    ///
    /// Called when recording stops. Returns `None` for a buffer without enough speech in it,
    /// rather than decoding it: this is the exact path that turned five seconds of room tone
    /// at the end of a recording into four invented `Okay.` segments. The clock still advances,
    /// so a later import of the same audio lines up.
    pub fn flush(&mut self) -> Option<Utterance> {
        if self.buffer.is_empty() {
            return None;
        }

        if self.gate_on_speech && self.speech_ms < self.config.min_speech_ms {
            self.discard_all();
            return None;
        }

        Some(self.take())
    }

    /// Take the buffer and advance the clock past it.
    fn take(&mut self) -> Utterance {
        self.take_until(self.buffer.len())
    }

    /// Take the buffer up to `cut`, keeping the rest to start the next one.
    ///
    /// The kept audio's speech is re-counted from the chunks it came from, rather than reset to
    /// zero: it is the middle of somebody talking, and if they stop right after the cut it must
    /// still be decoded rather than discarded as a quiet room.
    fn take_until(&mut self, cut: usize) -> Utterance {
        let cut = cut.min(self.buffer.len());
        let rest = self.buffer.split_off(cut);
        let samples = std::mem::replace(&mut self.buffer, rest);
        let offset_ms = self.offset_ms();
        self.offset_samples += samples.len() as u64;

        let mut consumed = samples.len();
        let mut kept = VecDeque::new();
        let mut speech_ms = 0;
        for (len, speech) in self.chunks.drain(..) {
            if consumed >= len {
                consumed -= len;
                continue;
            }
            let keep = len - consumed;
            // The chunk the cut fell inside keeps its speech in proportion to what is left of it.
            let share = speech * keep as i64 / len.max(1) as i64;
            consumed = 0;
            kept.push_back((keep, share));
            speech_ms += share;
        }
        self.chunks = kept;
        self.speech_ms = speech_ms;
        self.silence_ms = self.silence_ms.min(self.buffered_ms());

        Utterance { samples, offset_ms }
    }

    /// Where to cut a buffer that reached the length cap: the quietest moment near its end.
    ///
    /// Energy over 20 ms windows, stepped by 10 ms, across the last
    /// [`UtteranceConfig::cut_search_ms`] — never earlier than half way, so a cut cannot leave the
    /// decoder a sliver. The cut is the middle of the quietest window: a pause between clauses if
    /// there is one, a stop consonant or the gap between two words if not.
    fn quietest_cut(&self) -> usize {
        let len = self.buffer.len();
        let window = samples_for(20, self.sample_rate).max(1);
        let hop = (window / 2).max(1);
        let from = len
            .saturating_sub(samples_for(self.config.cut_search_ms, self.sample_rate))
            .max(len / 2);

        let mut best = (f64::INFINITY, len);
        let mut start = from;
        while start + window <= len {
            let energy: f64 = self.buffer[start..start + window]
                .iter()
                .map(|s| f64::from(*s) * f64::from(*s))
                .sum();
            if energy < best.0 {
                best = (energy, start + window / 2);
            }
            start += hop;
        }
        best.1
    }

    /// Drop buffered silence, keeping `preroll_ms` of it.
    ///
    /// `speech_ms` resets to zero even though the retained pre-roll may hold the tail of the
    /// blip being abandoned. Under-counting speech only ever delays a decode; over-counting
    /// would cause the bogus one this whole type exists to prevent.
    fn discard(&mut self) {
        let keep = samples_for(self.config.preroll_ms, self.sample_rate).min(self.buffer.len());
        let drop = self.buffer.len() - keep;
        if drop == 0 {
            return;
        }

        self.buffer.drain(..drop);
        self.offset_samples += drop as u64;
        self.speech_ms = 0;
        self.silence_ms = self.buffered_ms();
        self.chunks.clear();
        if keep > 0 {
            self.chunks.push_back((keep, 0));
        }
    }

    /// Drop the buffer entirely, advancing the clock past all of it.
    fn discard_all(&mut self) {
        self.offset_samples += self.buffer.len() as u64;
        self.buffer.clear();
        self.chunks.clear();
        self.speech_ms = 0;
        self.silence_ms = 0;
    }
}

fn ms_for(samples: usize, sample_rate: u32) -> i64 {
    if sample_rate == 0 {
        return 0;
    }
    (samples as i64 * 1000) / sample_rate as i64
}

fn samples_for(ms: i64, sample_rate: u32) -> usize {
    (ms.max(0) as u64 * sample_rate as u64 / 1000) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;
    /// The frame size a live microphone delivers.
    const FRAME_MS: i64 = 100;

    fn samples(ms: i64) -> usize {
        samples_for(ms, RATE)
    }

    /// Silence quiet enough that the gate's absolute floor rejects it outright.
    fn silence(ms: i64) -> Vec<f32> {
        vec![0.0; samples(ms)]
    }

    /// Voiced speech: a low-frequency tone, which has the energy and the low zero-crossing
    /// rate the gate looks for.
    fn speech(ms: i64) -> Vec<f32> {
        (0..samples(ms))
            .map(|i| {
                let t = i as f32 / RATE as f32;
                (t * 220.0 * std::f32::consts::TAU).sin() * 0.3
            })
            .collect()
    }

    /// Feed audio the way a microphone does — in 100 ms frames — collecting what comes out.
    fn feed(buffer: &mut UtteranceBuffer, audio: &[f32]) -> Vec<Utterance> {
        let mut out = Vec::new();
        for frame in audio.chunks(samples(FRAME_MS)) {
            if let Some(utterance) = buffer.push(frame) {
                out.push(utterance);
            }
        }
        out
    }

    /// The headline defect: with a ten-second fixed window, nothing reached the decoder for
    /// ten seconds. A phrase followed by a pause must be handed over shortly after the pause
    /// starts — about a second, not ten.
    #[test]
    fn a_phrase_is_handed_over_as_soon_as_the_speaker_pauses() {
        const PHRASE_MS: i64 = 1_500;
        let mut buffer = UtteranceBuffer::new(RATE);

        let mut audio = speech(PHRASE_MS);
        audio.extend(silence(1_500));
        let out = feed(&mut buffer, &audio);

        assert_eq!(out.len(), 1, "expected one utterance, got {}", out.len());
        assert_eq!(out[0].offset_ms, 0);

        // What a user perceives: how long after they stopped talking the decoder got the
        // phrase. The gate's hangover is part of this, which is why it is not just
        // `endpoint_silence_ms`.
        let latency_ms = out[0].duration_ms(RATE) - PHRASE_MS;
        assert!(
            latency_ms <= 1_000,
            "a phrase should reach the decoder within a second of the speaker stopping, \
             took {latency_ms} ms"
        );
        assert!(
            latency_ms >= 300,
            "handed over {latency_ms} ms after the phrase — too eager to call it an endpoint"
        );
    }

    /// The invented-text defect, reproduced exactly: speech, a long gap, then the recording
    /// stops with a tail of room tone in the buffer. That tail must never reach the model.
    #[test]
    fn a_silent_tail_at_the_end_of_a_recording_is_never_decoded() {
        let mut buffer = UtteranceBuffer::new(RATE);

        let mut audio = speech(2_000);
        audio.extend(silence(800));
        let mut out = feed(&mut buffer, &audio);
        assert_eq!(out.len(), 1, "the phrase itself should decode");

        // Five seconds of a quiet room, then stop. This is what produced four `Okay.`s.
        out.extend(feed(&mut buffer, &silence(5_000)));
        assert!(
            buffer.flush().is_none(),
            "room tone was handed to the decoder; this is the hallucination path"
        );
        assert_eq!(out.len(), 1, "silence produced an extra buffer to decode");
    }

    /// A blip in a long silence — a chair, a keyboard, a breath — is not an utterance.
    #[test]
    fn a_short_blip_in_a_quiet_room_is_not_worth_decoding() {
        let mut buffer = UtteranceBuffer::new(RATE);

        let mut audio = silence(1_000);
        audio.extend(speech(60));
        audio.extend(silence(2_000));

        let out = feed(&mut buffer, &audio);
        assert!(out.is_empty(), "a 60 ms blip should not be decoded");
        assert!(buffer.flush().is_none());
    }

    /// The phantom-speaker defect. The old code advanced its clock by the whole window, so a
    /// phrase in the first two seconds of a ten-second window was followed by an eight-second
    /// hole that diarization read as a change of speaker. Consecutive utterances must sit
    /// close to the speech that produced them.
    #[test]
    fn the_clock_tracks_real_speech_rather_than_window_boundaries() {
        let mut buffer = UtteranceBuffer::new(RATE);

        const TOTAL_MS: i64 = 1_000 + 1_200 + 1_000 + 1_200;

        let mut audio = speech(1_000);
        audio.extend(silence(1_200));
        audio.extend(speech(1_000));
        audio.extend(silence(1_200));

        let out = feed(&mut buffer, &audio);
        assert_eq!(out.len(), 2, "two phrases, two utterances");

        // The second utterance starts where the first ended — no invented hole. This is the
        // property the phantom speaker came from: with a fixed window the hole was 8000 ms.
        let first_end = out[0].offset_ms + out[0].duration_ms(RATE);
        let gap = out[1].offset_ms - first_end;
        assert!(
            gap.abs() <= FRAME_MS,
            "utterance 2 starts {gap} ms after utterance 1 ends; the clock has drifted"
        );

        // And the clock never runs past the audio actually fed.
        let total = out[1].offset_ms + out[1].duration_ms(RATE);
        assert!(
            total <= TOTAL_MS,
            "clock ran to {total} ms on {TOTAL_MS} ms of audio"
        );
        assert!(
            total >= TOTAL_MS - 1_000,
            "clock at {total} ms lost most of {TOTAL_MS} ms of audio"
        );
    }

    /// Someone reading a list aloud never pauses long enough to trip the endpoint. The
    /// transcript must not go quiet while they do it.
    #[test]
    fn unbroken_speech_still_produces_text_on_a_cap() {
        let mut buffer = UtteranceBuffer::with_config(
            RATE,
            UtteranceConfig {
                max_utterance_ms: 2_000,
                ..Default::default()
            },
        );

        let out = feed(&mut buffer, &speech(6_500));
        assert!(
            out.len() >= 3,
            "expected a buffer roughly every 2 s of unbroken speech, got {}",
            out.len()
        );

        // Consecutive, contiguous, and in order — a cut, not a gap.
        for pair in out.windows(2) {
            assert_eq!(
                pair[1].offset_ms,
                pair[0].offset_ms + pair[0].duration_ms(RATE),
                "capped buffers must abut"
            );
        }
    }

    /// Buffering a quiet room for an hour must not grow without bound.
    #[test]
    fn a_quiet_room_does_not_accumulate_audio() {
        let mut buffer = UtteranceBuffer::new(RATE);

        for _ in 0..100 {
            let out = feed(&mut buffer, &silence(1_000));
            assert!(out.is_empty());
        }

        assert!(
            buffer.buffered_ms() <= buffer.config().max_silence_ms + FRAME_MS,
            "held {} ms of silence",
            buffer.buffered_ms()
        );
        // The clock still advanced through all of it, so timings stay aligned.
        assert!(
            buffer.offset_ms() >= 99_000,
            "clock at {} ms after 100 s",
            buffer.offset_ms()
        );
    }

    /// Discarding silence must not eat the start of the next word.
    #[test]
    fn pre_roll_is_kept_so_a_soft_onset_is_not_clipped() {
        let mut buffer = UtteranceBuffer::new(RATE);

        feed(&mut buffer, &silence(3_000));
        let held = buffer.buffered_ms();
        assert!(
            held >= buffer.config().preroll_ms - FRAME_MS,
            "kept only {held} ms of pre-roll"
        );
    }

    /// A final phrase with no trailing pause — someone stops talking and clicks stop — is the
    /// last thing anyone said, and must not be lost.
    #[test]
    fn a_final_phrase_is_flushed_even_without_a_trailing_pause() {
        let mut buffer = UtteranceBuffer::new(RATE);

        let out = feed(&mut buffer, &speech(1_200));
        assert!(
            out.is_empty(),
            "no pause yet, so nothing should have decoded"
        );

        let flushed = buffer.flush().expect("the last phrase must survive");
        assert!(flushed.duration_ms(RATE) >= 1_000);
    }

    /// The diagnostic path still works: ungated, silence is handed over.
    #[test]
    fn the_gate_can_be_turned_off_for_diagnosis() {
        let mut buffer = UtteranceBuffer::with_config(
            RATE,
            UtteranceConfig {
                max_utterance_ms: 1_000,
                ..Default::default()
            },
        );
        buffer.disable_speech_gate();

        let out = feed(&mut buffer, &silence(3_000));
        assert!(!out.is_empty(), "ungated, silence should reach the decoder");
        assert!(buffer.flush().is_some() || buffer.buffered_ms() == 0);
    }

    /// A one-word answer is a real utterance and must reach the decoder.
    ///
    /// "Yes." is around 200 ms of voicing — below `min_speech_ms` on its own. It survives
    /// because the gate's hangover is *counted as speech*, which pads a short word past the
    /// threshold. That is load-bearing rather than lucky, so it is asserted here.
    #[test]
    fn an_isolated_one_word_answer_still_reaches_the_decoder() {
        let mut buffer = UtteranceBuffer::new(RATE);

        let mut audio = silence(400);
        audio.extend(speech(200));
        audio.extend(silence(1_500));

        let out = feed(&mut buffer, &audio);
        assert_eq!(
            out.len(),
            1,
            "a one-word answer was dropped: speech_ms reached {}",
            buffer.speech_ms()
        );
    }

    /// Speech with brief dips in it: the gaps between words and clauses that fluent speech has,
    /// far too short for the gate to call a pause.
    fn speech_with_dips(total_ms: i64, dips_at: &[i64], dip_ms: i64) -> Vec<f32> {
        let mut audio = speech(total_ms);
        for at in dips_at {
            let end = samples(at + dip_ms).min(audio.len());
            for sample in &mut audio[samples(*at)..end] {
                *sample *= 0.02;
            }
        }
        audio
    }

    /// The defect behind "so the margin story" vanishing from a real transcript: cut exactly at the
    /// cap, a word straddling the cut is dropped from both halves. The cut belongs at the quietest
    /// moment before the cap, and what follows it must begin the next buffer.
    #[test]
    fn a_length_cut_lands_between_words_rather_than_through_one() {
        let mut buffer = UtteranceBuffer::new(RATE);

        // A 120 ms dip at 6.9 s: inside the two seconds before the 8 s cap, and far too short for
        // the gate to call a pause.
        let out = feed(&mut buffer, &speech_with_dips(9_000, &[6_900], 120));

        assert_eq!(out.len(), 1, "one cut in nine seconds of speech");
        let cut_ms = out[0].duration_ms(RATE);
        assert!(
            (6_900..=7_020).contains(&cut_ms),
            "cut at {cut_ms} ms, through a word, rather than in the dip at 6900..7020 ms"
        );

        // Nothing lost or repeated: what followed the cut is still held, and the clock is at it.
        assert_eq!(buffer.offset_ms(), cut_ms);
        assert_eq!(cut_ms + buffer.buffered_ms(), 9_000);
    }

    /// What follows a cut is the middle of somebody talking. If they stop straight after it, it is
    /// the rest of their phrase, and it must be decoded rather than discarded as a quiet room.
    #[test]
    fn speech_kept_after_a_cut_is_decoded_when_the_speaker_stops() {
        let mut buffer = UtteranceBuffer::new(RATE);

        let mut audio = speech_with_dips(8_000, &[6_900], 120);
        audio.extend(silence(2_500));
        let out = feed(&mut buffer, &audio);

        assert_eq!(
            out.len(),
            2,
            "the end of the phrase after the cut was dropped"
        );
        assert!(
            out[1].duration_ms(RATE) >= 1_000,
            "the second buffer holds only {} ms",
            out[1].duration_ms(RATE)
        );
        assert_eq!(
            out[1].offset_ms,
            out[0].offset_ms + out[0].duration_ms(RATE),
            "the two halves must abut"
        );
    }

    /// Cuts land on arbitrary samples, so the clock is kept in samples. Rounding each cut to a
    /// millisecond would drift a long meeting's timings by one per cut.
    #[test]
    fn the_clock_does_not_drift_across_many_cuts() {
        let mut buffer = UtteranceBuffer::with_config(
            RATE,
            UtteranceConfig {
                max_utterance_ms: 1_000,
                cut_search_ms: 500,
                ..Default::default()
            },
        );

        // Frames of an awkward length, so no cut falls on a whole millisecond by accident.
        let audio = speech(60_000);
        let mut consumed = 0usize;
        for frame in audio.chunks(1_237) {
            if let Some(utterance) = buffer.push(frame) {
                consumed += utterance.samples.len();
            }
        }

        let expected_ms = (consumed as u64 * 1000 / RATE as u64) as i64;
        assert_eq!(buffer.offset_ms(), expected_ms);
    }

    #[test]
    fn an_empty_buffer_has_nothing_to_flush() {
        let mut buffer = UtteranceBuffer::new(RATE);
        assert!(buffer.flush().is_none());
        assert!(buffer.push(&[]).is_none());
    }
}
