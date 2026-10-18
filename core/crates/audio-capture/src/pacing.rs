//! Keeping a live stream that can fall silent on the wall clock.
//!
//! # Why a source needs this
//!
//! ScreenCaptureKit delivers system audio only while something is playing: a silent machine
//! produces no callbacks at all. A source that only ever handed over what arrived therefore had
//! two bad choices when nothing did, and [`crate::SystemAudioSource`] took both:
//!
//! - **It blocked.** Each read waited half a second for audio that was never coming. Recording a
//!   call reads the microphone and the system tap in turn, so every turn waited on the silent
//!   tap while the microphone delivered five times as much as was being read — the microphone's
//!   transcript fell further behind the room for as long as the far end was quiet.
//! - **Its clock stood still.** What it handed back after waiting was an empty frame, and a
//!   timestamp counted from samples does not move for an empty frame. After five silent minutes
//!   the next thing the far end said was stamped as if it came straight after the last thing,
//!   five minutes before it was said, and sorted into the transcript there.
//!
//! [`PacedStream`] fills the stretch the platform did not deliver with silence, once the wall
//! clock says it is overdue. A silent tap then reads like a quiet microphone: one frame per frame
//! of real time, stamped when it happened.

use std::time::Duration;

/// Audio arriving on a platform callback, re-cut into frames that keep time with the wall clock.
#[derive(Debug)]
pub(crate) struct PacedStream {
    rate: u32,
    /// How late audio may be before the gap is filled with silence.
    ///
    /// Buffers spend a little time in flight between capture and callback. Filling silence the
    /// moment a frame's real time has passed would overtake audio that is merely on its way, and
    /// that audio would then land after the silence, late. Waiting this long first means only
    /// audio that is genuinely not coming is replaced.
    allowance: Duration,
    /// Arrived, and not yet handed out.
    pending: Vec<f32>,
    /// Samples handed out so far: the stream's own clock.
    emitted: u64,
}

impl PacedStream {
    pub(crate) fn new(rate: u32, allowance: Duration) -> Self {
        Self {
            rate,
            allowance,
            pending: Vec::new(),
            emitted: 0,
        }
    }

    /// Audio the platform delivered.
    pub(crate) fn push(&mut self, samples: &[f32]) {
        self.pending.extend_from_slice(samples);
    }

    /// The next whole frame, if there is one by `elapsed` since the clock's origin.
    ///
    /// Real audio when enough has arrived. Otherwise, once the frame's real time is more than the
    /// allowance in the past, whatever did arrive topped up with silence: that stretch was not
    /// delivered because nothing was playing.
    ///
    /// Returns the frame's samples and the stream position, in samples, it starts at.
    pub(crate) fn frame(&mut self, elapsed: Duration, frame_len: usize) -> Option<(Vec<f32>, u64)> {
        let frame_len = frame_len.max(1);
        if self.pending.len() >= frame_len {
            return Some(self.take(frame_len));
        }

        let due = self.samples_in(elapsed.saturating_sub(self.allowance));
        if due >= self.emitted + frame_len as u64 {
            self.pending.resize(frame_len, 0.0);
            return Some(self.take(frame_len));
        }
        None
    }

    /// What is left once capture has stopped: real audio only, a frame at a time, then `None`.
    ///
    /// No silence is filled after a stop. The recording is over, and padding it out would only
    /// lengthen a meeting with time nobody spent in it.
    pub(crate) fn rest(&mut self, frame_len: usize) -> Option<(Vec<f32>, u64)> {
        if self.pending.is_empty() {
            return None;
        }
        let take = frame_len.max(1).min(self.pending.len());
        Some(self.take(take))
    }

    /// Audio arrived and not yet handed out, in samples. For diagnostics: the audio itself must
    /// not reach a log line.
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Milliseconds since the origin at which the stream position `at` falls.
    pub(crate) fn ms_at(&self, at: u64) -> i64 {
        (at * 1000 / u64::from(self.rate.max(1))) as i64
    }

    fn take(&mut self, len: usize) -> (Vec<f32>, u64) {
        let samples: Vec<f32> = self.pending.drain(..len).collect();
        let at = self.emitted;
        self.emitted += samples.len() as u64;
        (samples, at)
    }

    fn samples_in(&self, span: Duration) -> u64 {
        (span.as_secs_f64() * f64::from(self.rate)) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;
    const FRAME: usize = 1_600; // 100 ms
    const ALLOWANCE: Duration = Duration::from_millis(150);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// While audio keeps arriving the stream is exactly that audio, with nothing invented.
    #[test]
    fn delivered_audio_passes_through_untouched() {
        let mut stream = PacedStream::new(RATE, ALLOWANCE);
        let audio: Vec<f32> = (0..FRAME * 3).map(|i| (i % 7) as f32 / 10.0).collect();

        stream.push(&audio);
        let mut out = Vec::new();
        while let Some((samples, at)) = stream.frame(ms(300), FRAME) {
            assert_eq!(at as usize, out.len(), "frames must abut");
            out.extend(samples);
        }
        assert_eq!(out, audio);
    }

    /// The defect: a silent machine delivers nothing, and the stream must keep time anyway — a
    /// frame of silence per frame of real time, stamped when it happened.
    #[test]
    fn a_silent_platform_still_yields_a_frame_per_frame_of_real_time() {
        let mut stream = PacedStream::new(RATE, ALLOWANCE);

        // Ten seconds of nothing being delivered, read the way a pipeline reads.
        let mut frames = 0;
        let mut last_at = None;
        for tick in 0..=100u64 {
            while let Some((samples, at)) = stream.frame(ms(tick * 100), FRAME) {
                assert!(samples.iter().all(|s| *s == 0.0));
                assert_eq!(samples.len(), FRAME);
                last_at = Some(at);
                frames += 1;
            }
        }

        // 10 s less the allowance, in whole frames.
        assert_eq!(frames, (10_000 - 150) / 100);
        let last = last_at.expect("frames");
        assert_eq!(
            stream.ms_at(last),
            9_700,
            "the clock must advance through silence"
        );
    }

    /// Nothing is filled in before its time: audio still in flight is not overtaken.
    #[test]
    fn silence_is_not_filled_in_before_the_allowance_has_passed() {
        let mut stream = PacedStream::new(RATE, ALLOWANCE);
        assert!(
            stream.frame(ms(240), FRAME).is_none(),
            "100 ms is not overdue at 240 ms"
        );
        assert!(stream.frame(ms(250), FRAME).is_some(), "it is at 250 ms");
    }

    /// When the far end speaks again after a silence, their words land after the silence — at
    /// the time they were said, not straight after whatever came before the gap.
    #[test]
    fn audio_after_a_gap_is_placed_after_the_silence() {
        let mut stream = PacedStream::new(RATE, ALLOWANCE);

        stream.push(&vec![0.5; FRAME]);
        let (_, first) = stream.frame(ms(100), FRAME).expect("first frame");
        assert_eq!(first, 0);

        // Five seconds of nothing.
        for tick in 2..=50u64 {
            while stream.frame(ms(tick * 100), FRAME).is_some() {}
        }

        stream.push(&vec![0.5; FRAME]);
        let (samples, at) = stream.frame(ms(5_100), FRAME).expect("the resumed audio");
        assert!(samples.iter().all(|s| *s == 0.5), "this is the real audio");
        let at_ms = stream.ms_at(at);
        assert!(
            (4_700..=5_000).contains(&at_ms),
            "resumed audio stamped at {at_ms} ms; it was said at about 5000 ms"
        );
    }

    /// A partial frame that is overdue is the audio that came, then silence — not dropped.
    #[test]
    fn an_overdue_partial_frame_keeps_what_arrived() {
        let mut stream = PacedStream::new(RATE, ALLOWANCE);
        stream.push(&vec![0.25; 400]);

        let (samples, _) = stream.frame(ms(400), FRAME).expect("overdue");
        assert_eq!(samples.len(), FRAME);
        assert!(samples[..400].iter().all(|s| *s == 0.25));
        assert!(samples[400..].iter().all(|s| *s == 0.0));
    }

    /// After a stop only real audio comes out, and then nothing.
    #[test]
    fn the_rest_after_a_stop_is_only_what_arrived() {
        let mut stream = PacedStream::new(RATE, ALLOWANCE);
        stream.push(&vec![0.1; FRAME + 300]);

        assert_eq!(stream.rest(FRAME).map(|(s, _)| s.len()), Some(FRAME));
        assert_eq!(stream.rest(FRAME).map(|(s, _)| s.len()), Some(300));
        assert!(stream.rest(FRAME).is_none());
    }
}
