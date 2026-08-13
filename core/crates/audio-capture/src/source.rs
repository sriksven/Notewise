//! Audio sources.

use serde::{Deserialize, Serialize};

use crate::format::{AudioFormat, SampleRate};
use crate::{AudioFrame, CaptureError, Result};

/// What to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKind {
    /// This machine's microphone — the local participant.
    Microphone,
    /// System/loopback audio — the remote participants.
    SystemAudio,
    /// Both, mixed. What a normal meeting recording needs.
    Combined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureConfig {
    pub kind: CaptureKind,
    /// Preferred device, by name. `None` means the system default.
    pub device: Option<String>,
    /// Frame size in milliseconds.
    ///
    /// A tradeoff: smaller frames mean lower latency to the first transcript segment, larger
    /// frames mean fewer wakeups. 100 ms is comfortably below the point where a user
    /// perceives lag while keeping overhead negligible.
    pub frame_ms: u32,
    pub format: AudioFormat,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            kind: CaptureKind::Combined,
            device: None,
            frame_ms: 100,
            format: AudioFormat::transcription(),
        }
    }
}

impl CaptureConfig {
    /// Samples per frame, accounting for channel count.
    pub fn samples_per_frame(&self) -> usize {
        let per_channel =
            (self.format.sample_rate.hz() as u64 * self.frame_ms as u64 / 1000) as usize;
        per_channel * self.format.channels.max(1) as usize
    }
}

/// A stream of audio frames.
///
/// Pull-based rather than callback-based: the caller controls pacing and backpressure, and
/// there is no risk of a slow consumer blocking an OS audio thread — which on most platforms
/// causes dropouts rather than merely lag.
pub trait AudioSource: std::fmt::Debug + Send {
    fn format(&self) -> AudioFormat;

    /// Next frame, or `None` when the source is exhausted.
    ///
    /// A live device blocks until a frame is ready; a file returns immediately.
    fn next_frame(&mut self) -> Result<Option<AudioFrame>>;

    /// Stop capturing and release the device.
    fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    /// Whether this source produces audio in real time.
    ///
    /// `false` for files, which the transcription pipeline can consume as fast as it manages.
    fn is_realtime(&self) -> bool {
        true
    }
}

/// The shape of a synthetic signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Waveform {
    Silence,
    /// A sine tone, for verifying the pipeline carries a signal end to end.
    Sine {
        hz: u32,
    },
}

/// A generated source.
///
/// Real and works everywhere. This is what lets the transcription and diarization layers
/// above be developed and tested without audio hardware, a permission grant, or a fixture file.
#[derive(Debug)]
pub struct SyntheticSource {
    format: AudioFormat,
    waveform: Waveform,
    samples_per_frame: usize,
    frames_remaining: usize,
    position: u64,
}

impl SyntheticSource {
    /// A source producing `duration_ms` of the given waveform.
    pub fn new(waveform: Waveform, duration_ms: u32, config: &CaptureConfig) -> Self {
        let frames = (duration_ms / config.frame_ms.max(1)) as usize;
        Self {
            format: config.format,
            waveform,
            samples_per_frame: config.samples_per_frame(),
            frames_remaining: frames,
            position: 0,
        }
    }

    /// One second of silence in transcription format. A convenient default in tests.
    pub fn silence() -> Self {
        Self::new(Waveform::Silence, 1000, &CaptureConfig::default())
    }

    fn sample_at(&self, index: u64) -> f32 {
        match self.waveform {
            Waveform::Silence => 0.0,
            Waveform::Sine { hz } => {
                let t = index as f64 / self.format.sample_rate.hz() as f64;
                (t * hz as f64 * std::f64::consts::TAU).sin() as f32
            }
        }
    }
}

impl AudioSource for SyntheticSource {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn next_frame(&mut self) -> Result<Option<AudioFrame>> {
        if self.frames_remaining == 0 {
            return Ok(None);
        }
        self.frames_remaining -= 1;

        let samples: Vec<f32> = (0..self.samples_per_frame)
            .map(|i| self.sample_at(self.position + i as u64))
            .collect();

        let timestamp_ms =
            (self.position as i64 * 1000) / self.format.sample_rate.hz().max(1) as i64;
        self.position += self.samples_per_frame as u64;

        Ok(Some(AudioFrame::new(samples, self.format, timestamp_ms)))
    }

    fn is_realtime(&self) -> bool {
        false
    }
}

/// Audio read from a file.
///
/// Backs the import path — a meeting recorded elsewhere and brought in afterward. Currently
/// reads uncompressed 32-bit float WAV; compressed formats need a decoder dependency.
#[derive(Debug)]
pub struct FileSource {
    samples: Vec<f32>,
    format: AudioFormat,
    samples_per_frame: usize,
    position: usize,
}

impl FileSource {
    /// A source over already-decoded samples.
    pub fn from_samples(samples: Vec<f32>, format: AudioFormat, frame_ms: u32) -> Self {
        let per_channel = (format.sample_rate.hz() as u64 * frame_ms.max(1) as u64 / 1000) as usize;
        Self {
            samples,
            format,
            samples_per_frame: (per_channel * format.channels.max(1) as usize).max(1),
            position: 0,
        }
    }

    /// Read a 32-bit float WAV file.
    pub fn open_wav(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let (samples, format) = decode_f32_wav(&bytes)?;
        Ok(Self::from_samples(samples, format, 100))
    }

    pub fn remaining_samples(&self) -> usize {
        self.samples.len().saturating_sub(self.position)
    }
}

impl AudioSource for FileSource {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn next_frame(&mut self) -> Result<Option<AudioFrame>> {
        if self.position >= self.samples.len() {
            return Ok(None);
        }

        let end = (self.position + self.samples_per_frame).min(self.samples.len());
        let chunk = self.samples[self.position..end].to_vec();

        let frames_elapsed = self.position / self.format.channels.max(1) as usize;
        let timestamp_ms =
            (frames_elapsed as i64 * 1000) / self.format.sample_rate.hz().max(1) as i64;
        self.position = end;

        Ok(Some(AudioFrame::new(chunk, self.format, timestamp_ms)))
    }

    fn is_realtime(&self) -> bool {
        false
    }
}

/// Minimal 32-bit float WAV decoder.
///
/// Hand-written rather than pulled from a crate because it handles exactly one format and
/// the alternative is a dependency for ~40 lines. Anything else is rejected explicitly.
fn decode_f32_wav(bytes: &[u8]) -> Result<(Vec<f32>, AudioFormat)> {
    fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
        Some(u16::from_le_bytes(
            bytes.get(offset..offset + 2)?.try_into().ok()?,
        ))
    }
    fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
    }

    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(CaptureError::BadFormat("not a RIFF/WAVE file".into()));
    }

    // Walk the chunk list rather than assuming a fixed layout — real encoders insert
    // LIST/fact chunks before the data.
    let mut offset = 12;
    let mut format = None;
    let mut data_range = None;

    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32_at(bytes, offset + 4)
            .ok_or_else(|| CaptureError::BadFormat("truncated chunk header".into()))?
            as usize;
        let body = offset + 8;

        match id {
            b"fmt " => {
                let audio_format = u16_at(bytes, body)
                    .ok_or_else(|| CaptureError::BadFormat("truncated fmt chunk".into()))?;
                let channels = u16_at(bytes, body + 2).unwrap_or(0);
                let sample_rate = u32_at(bytes, body + 4).unwrap_or(0);
                let bits = u16_at(bytes, body + 14).unwrap_or(0);

                // 3 = IEEE float.
                if audio_format != 3 || bits != 32 {
                    return Err(CaptureError::BadFormat(format!(
                        "expected 32-bit float PCM (format 3), got format {audio_format} at \
                         {bits} bits"
                    )));
                }
                format = Some(AudioFormat::new(SampleRate::from_hz(sample_rate), channels));
            }
            b"data" => data_range = Some((body, (body + size).min(bytes.len()))),
            _ => {}
        }

        // Chunks are word-aligned; an odd size is followed by a pad byte.
        offset = body + size + (size % 2);
    }

    let format = format.ok_or_else(|| CaptureError::BadFormat("no fmt chunk".into()))?;
    let (start, end) = data_range.ok_or_else(|| CaptureError::BadFormat("no data chunk".into()))?;

    let samples = bytes[start..end]
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    Ok((samples, format))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_size_accounts_for_rate_and_channels() {
        let config = CaptureConfig::default(); // 16 kHz mono, 100 ms
        assert_eq!(config.samples_per_frame(), 1600);

        let stereo = CaptureConfig {
            format: AudioFormat::new(SampleRate::STUDIO, 2),
            ..Default::default()
        };
        assert_eq!(stereo.samples_per_frame(), 4800 * 2);
    }

    #[test]
    fn a_synthetic_source_yields_the_requested_duration() {
        let config = CaptureConfig::default();
        let mut source = SyntheticSource::new(Waveform::Silence, 1000, &config);

        let mut total_ms = 0;
        let mut frames = 0;
        while let Some(frame) = source.next_frame().unwrap() {
            total_ms += frame.duration_ms();
            frames += 1;
        }

        assert_eq!(frames, 10, "1000ms at 100ms frames");
        assert_eq!(total_ms, 1000);
    }

    #[test]
    fn a_source_returns_none_once_exhausted() {
        let mut source = SyntheticSource::silence();
        while source.next_frame().unwrap().is_some() {}

        assert!(source.next_frame().unwrap().is_none());
        assert!(
            source.next_frame().unwrap().is_none(),
            "and stays exhausted"
        );
    }

    #[test]
    fn timestamps_advance_monotonically_without_gaps() {
        let mut source = SyntheticSource::silence();
        let mut expected = 0;

        while let Some(frame) = source.next_frame().unwrap() {
            assert_eq!(
                frame.timestamp_ms, expected,
                "a gap here would misalign the whole transcript"
            );
            expected += frame.duration_ms();
        }
    }

    #[test]
    fn silence_is_actually_silent() {
        let mut source = SyntheticSource::silence();
        let frame = source.next_frame().unwrap().unwrap();
        assert_eq!(crate::rms(&frame.samples), 0.0);
    }

    #[test]
    fn a_sine_source_carries_a_measurable_signal() {
        let config = CaptureConfig::default();
        let mut source = SyntheticSource::new(Waveform::Sine { hz: 440 }, 1000, &config);
        let frame = source.next_frame().unwrap().unwrap();

        let level = crate::rms(&frame.samples);
        // A full-scale sine has RMS ~0.707.
        assert!((0.6..0.8).contains(&level), "rms was {level}");
    }

    #[test]
    fn a_file_source_yields_every_sample_exactly_once() {
        let samples: Vec<f32> = (0..5000).map(|i| i as f32).collect();
        let mut source =
            FileSource::from_samples(samples.clone(), AudioFormat::transcription(), 100);

        let mut collected = Vec::new();
        while let Some(frame) = source.next_frame().unwrap() {
            collected.extend(frame.samples);
        }

        assert_eq!(collected, samples);
        assert_eq!(source.remaining_samples(), 0);
    }

    #[test]
    fn a_file_source_is_not_realtime() {
        // The pipeline may consume an imported file as fast as it manages.
        let source = FileSource::from_samples(vec![0.0; 10], AudioFormat::transcription(), 100);
        assert!(!source.is_realtime());
    }

    #[test]
    fn a_final_partial_frame_is_still_delivered() {
        // 1700 samples at 1600 per frame: the trailing 100 must not be dropped.
        let mut source =
            FileSource::from_samples(vec![0.1; 1700], AudioFormat::transcription(), 100);

        let first = source.next_frame().unwrap().unwrap();
        let second = source.next_frame().unwrap().unwrap();

        assert_eq!(first.samples.len(), 1600);
        assert_eq!(second.samples.len(), 100, "tail audio must not be lost");
        assert!(source.next_frame().unwrap().is_none());
    }

    /// Build a minimal 32-bit float WAV in memory.
    fn wav(samples: &[f32], channels: u16, rate: u32) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = Vec::new();

        out.extend(b"RIFF");
        out.extend(((36 + data.len()) as u32).to_le_bytes());
        out.extend(b"WAVE");

        out.extend(b"fmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(3u16.to_le_bytes()); // IEEE float
        out.extend(channels.to_le_bytes());
        out.extend(rate.to_le_bytes());
        out.extend((rate * channels as u32 * 4).to_le_bytes());
        out.extend((channels * 4).to_le_bytes());
        out.extend(32u16.to_le_bytes());

        out.extend(b"data");
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
        out
    }

    #[test]
    fn a_float_wav_decodes_to_its_samples() {
        let samples = vec![0.0, 0.5, -0.5, 1.0];
        let (decoded, format) = decode_f32_wav(&wav(&samples, 1, 16_000)).unwrap();

        assert_eq!(decoded, samples);
        assert_eq!(format, AudioFormat::transcription());
    }

    #[test]
    fn stereo_wav_metadata_is_read_correctly() {
        let (_, format) = decode_f32_wav(&wav(&[0.0; 8], 2, 48_000)).unwrap();
        assert_eq!(format, AudioFormat::new(SampleRate::STUDIO, 2));
    }

    #[test]
    fn a_wav_with_an_extra_chunk_still_decodes() {
        // Real encoders insert LIST/fact chunks; assuming a fixed 44-byte header breaks.
        let mut bytes = wav(&[0.25, 0.5], 1, 16_000);
        let list = {
            let mut chunk = Vec::new();
            chunk.extend(b"LIST");
            chunk.extend(4u32.to_le_bytes());
            chunk.extend(b"INFO");
            chunk
        };
        bytes.splice(12..12, list);

        let (decoded, _) = decode_f32_wav(&bytes).expect("should skip unknown chunks");
        assert_eq!(decoded, vec![0.25, 0.5]);
    }

    #[test]
    fn non_wav_input_is_rejected() {
        assert!(matches!(
            decode_f32_wav(b"this is not a wav file at all, not even close ok").unwrap_err(),
            CaptureError::BadFormat(_)
        ));
    }

    #[test]
    fn integer_wav_is_rejected_with_a_specific_reason() {
        let mut bytes = wav(&[0.0; 4], 1, 16_000);
        bytes[20] = 1; // format 1 = integer PCM
        bytes[34] = 16; // 16-bit

        let err = decode_f32_wav(&bytes).expect_err("integer PCM is not supported");
        let message = err.to_string();
        assert!(message.contains("32-bit float"), "{message}");
    }

    #[test]
    fn sources_are_usable_behind_a_trait_object() {
        let mut sources: Vec<Box<dyn AudioSource>> = vec![
            Box::new(SyntheticSource::silence()),
            Box::new(FileSource::from_samples(
                vec![0.0; 100],
                AudioFormat::transcription(),
                100,
            )),
        ];

        for source in &mut sources {
            assert!(source.next_frame().unwrap().is_some());
            assert!(source.stop().is_ok());
        }
    }
}
