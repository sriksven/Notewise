//! Writing captured audio to a file as it arrives.
//!
//! # Why streaming, and why the header is patched at the end
//!
//! A RIFF header states the size of the file and of its data chunk, and neither is known while a
//! meeting is still being recorded. Buffering until it is would mean holding the whole recording in
//! memory — an hour of 16 kHz mono float is over two hundred megabytes — so the header is written
//! with zeroed sizes, samples are appended as they arrive, and the two length fields are patched
//! once on [`WavWriter::finish`].
//!
//! # What an interrupted recording leaves behind
//!
//! A crash before `finish` leaves a file whose header claims zero samples. That is a deliberate
//! choice between two bad options: the alternative is rewriting the header on every frame, which is
//! a seek and a write per frame for the whole meeting. A zero-length recording is recognisably
//! broken, which is better than a plausible file of the wrong length — and the transcript, which is
//! what the product is actually for, was written to the database as it went.
//!
//! The samples are all there on disk regardless, so a file like this is recoverable by patching two
//! integers. [`repair`] does exactly that.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::format::AudioFormat;
use crate::CaptureError;

type Result<T> = std::result::Result<T, CaptureError>;

/// Bytes before the data chunk's payload. Fixed, because this writer emits no optional chunks.
const HEADER_LEN: u64 = 44;

/// Offset of the RIFF chunk size field.
const RIFF_SIZE_AT: u64 = 4;

/// Offset of the data chunk size field.
const DATA_SIZE_AT: u64 = 40;

/// Writes 32-bit float WAV incrementally.
#[derive(Debug)]
pub struct WavWriter {
    out: BufWriter<File>,
    path: PathBuf,
    bytes_written: u64,
}

impl WavWriter {
    /// Create a file and write a header with placeholder sizes.
    pub fn create(path: impl AsRef<Path>, format: AudioFormat) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CaptureError::BadFormat(format!("could not create {}: {e}", parent.display()))
            })?;
        }

        let file = File::create(&path).map_err(|e| {
            CaptureError::BadFormat(format!("could not create {}: {e}", path.display()))
        })?;
        let mut out = BufWriter::new(file);
        write_header(&mut out, format, 0)?;

        Ok(Self {
            out,
            path,
            bytes_written: 0,
        })
    }

    /// Append one frame's samples, interleaved as the format describes.
    pub fn write_frame(&mut self, samples: &[f32]) -> Result<()> {
        for sample in samples {
            self.out
                .write_all(&sample.to_le_bytes())
                .map_err(|e| CaptureError::BadFormat(format!("could not write audio: {e}")))?;
        }
        self.bytes_written += (samples.len() * 4) as u64;
        Ok(())
    }

    /// How much audio payload has been written.
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written + HEADER_LEN
    }

    /// Patch the two length fields and close the file.
    pub fn finish(mut self) -> Result<u64> {
        self.out
            .flush()
            .map_err(|e| CaptureError::BadFormat(format!("could not flush audio: {e}")))?;
        let mut file = self
            .out
            .into_inner()
            .map_err(|e| CaptureError::BadFormat(format!("could not finish audio: {e}")))?;

        patch_sizes(&mut file, self.bytes_written)?;
        Ok(self.bytes_written + HEADER_LEN)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Fix the length fields of a file whose writer never finished.
///
/// The samples are on disk either way; only the two integers are wrong. Returns the total size.
pub fn repair(path: impl AsRef<Path>) -> Result<u64> {
    let path = path.as_ref();
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| CaptureError::BadFormat(format!("could not open {}: {e}", path.display())))?;

    let total = file
        .metadata()
        .map_err(|e| CaptureError::BadFormat(format!("could not stat {}: {e}", path.display())))?
        .len();
    if total < HEADER_LEN {
        return Err(CaptureError::BadFormat(format!(
            "{} is too short to be a WAV file",
            path.display()
        )));
    }

    patch_sizes(&mut file, total - HEADER_LEN)?;
    Ok(total)
}

fn patch_sizes(file: &mut File, data_len: u64) -> Result<()> {
    let write_at = |file: &mut File, at: u64, value: u32| -> Result<()> {
        file.seek(SeekFrom::Start(at))
            .map_err(|e| CaptureError::BadFormat(format!("could not seek audio: {e}")))?;
        file.write_all(&value.to_le_bytes())
            .map_err(|e| CaptureError::BadFormat(format!("could not patch audio header: {e}")))
    };

    // Saturating, so a recording longer than 4 GiB produces a header claiming the maximum rather
    // than wrapping to something small. RIFF cannot describe it either way; a truncated-looking
    // file is the honest failure.
    let data = u32::try_from(data_len).unwrap_or(u32::MAX);
    let riff = u32::try_from(data_len + HEADER_LEN - 8).unwrap_or(u32::MAX);

    write_at(file, RIFF_SIZE_AT, riff)?;
    write_at(file, DATA_SIZE_AT, data)?;
    file.flush()
        .map_err(|e| CaptureError::BadFormat(format!("could not flush audio: {e}")))?;
    Ok(())
}

fn write_header(out: &mut impl Write, format: AudioFormat, data_len: u32) -> Result<()> {
    let channels = format.channels;
    let rate = format.sample_rate.hz();
    let bytes_per_sample = 4u32;
    let block_align = u32::from(channels) * bytes_per_sample;

    let mut header = Vec::with_capacity(HEADER_LEN as usize);
    header.extend(b"RIFF");
    header.extend((data_len + 36).to_le_bytes());
    header.extend(b"WAVE");

    header.extend(b"fmt ");
    header.extend(16u32.to_le_bytes());
    // 3 is IEEE float, matching what `decode_wav` reads and what the pipeline works in natively —
    // there is no conversion here, so nothing to lose.
    header.extend(3u16.to_le_bytes());
    header.extend(channels.to_le_bytes());
    header.extend(rate.to_le_bytes());
    header.extend((rate * block_align).to_le_bytes());
    header.extend(u16::try_from(block_align).unwrap_or(u16::MAX).to_le_bytes());
    header.extend(32u16.to_le_bytes());

    header.extend(b"data");
    header.extend(data_len.to_le_bytes());

    debug_assert_eq!(header.len() as u64, HEADER_LEN);
    out.write_all(&header)
        .map_err(|e| CaptureError::BadFormat(format!("could not write audio header: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::FileSource;
    use crate::AudioSource;

    fn round_trip(samples: &[f32], format: AudioFormat) -> Vec<f32> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("out.wav");

        let mut writer = WavWriter::create(&path, format).expect("create");
        // In frames, as capture delivers it, so the incremental path is what is under test.
        for chunk in samples.chunks(3) {
            writer.write_frame(chunk).expect("write");
        }
        writer.finish().expect("finish");

        let mut source = FileSource::open_wav(&path).expect("read back");
        let mut read = Vec::new();
        while let Some(frame) = source.next_frame().expect("frame") {
            read.extend_from_slice(&frame.samples);
        }
        read
    }

    #[test]
    fn what_was_written_is_what_reads_back() {
        let samples: Vec<f32> = (0..64).map(|n| (n as f32 / 64.0) - 0.5).collect();
        let read = round_trip(&samples, AudioFormat::transcription());
        assert_eq!(read, samples);
    }

    #[test]
    fn an_empty_recording_is_still_a_valid_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("empty.wav");

        let writer = WavWriter::create(&path, AudioFormat::transcription()).expect("create");
        let total = writer.finish().expect("finish");

        assert_eq!(total, HEADER_LEN);
        assert!(
            FileSource::open_wav(&path).is_ok(),
            "a meeting that captured nothing must not leave a file that cannot be opened"
        );
    }

    #[test]
    fn the_directory_is_created_if_it_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/deeper/out.wav");

        let mut writer = WavWriter::create(&path, AudioFormat::transcription()).expect("create");
        writer.write_frame(&[0.25]).expect("write");
        writer.finish().expect("finish");

        assert!(path.is_file());
    }

    /// The interrupted case: samples on disk, header claiming zero. Recoverable by patching two
    /// integers, which is the whole reason the writer is allowed to defer them.
    #[test]
    fn a_recording_whose_writer_never_finished_can_be_repaired() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("crashed.wav");
        let samples: Vec<f32> = (0..32).map(|n| n as f32 / 32.0).collect();

        {
            let mut writer =
                WavWriter::create(&path, AudioFormat::transcription()).expect("create");
            for chunk in samples.chunks(4) {
                writer.write_frame(chunk).expect("write");
            }
            // Dropped without `finish`: flushed by BufWriter's Drop, header never patched.
        }

        // Before repair the header says there is no audio, so a reader sees none.
        let mut before = FileSource::open_wav(&path).expect("still openable");
        let mut read_before = Vec::new();
        while let Some(frame) = before.next_frame().expect("frame") {
            read_before.extend_from_slice(&frame.samples);
        }
        assert!(read_before.is_empty(), "the header claims zero samples");

        repair(&path).expect("repair");

        let mut after = FileSource::open_wav(&path).expect("read back");
        let mut read_after = Vec::new();
        while let Some(frame) = after.next_frame().expect("frame") {
            read_after.extend_from_slice(&frame.samples);
        }
        assert_eq!(read_after, samples, "every sample was on disk all along");
    }

    #[test]
    fn repairing_something_that_is_not_a_wav_file_is_reported_not_panicked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tiny");
        std::fs::write(&path, b"no").expect("write");

        assert!(repair(&path).is_err());
    }

    #[test]
    fn stereo_metadata_survives_the_round_trip() {
        let format = AudioFormat::new(crate::SampleRate::STUDIO, 2);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("stereo.wav");

        let mut writer = WavWriter::create(&path, format).expect("create");
        writer.write_frame(&[0.1, 0.2, 0.3, 0.4]).expect("write");
        writer.finish().expect("finish");

        let source = FileSource::open_wav(&path).expect("read back");
        assert_eq!(source.format(), format);
    }
}
