//! Speaker embeddings from an ONNX model.
//!
//! Turns a span of audio into a fixed-length vector where distance means "different person".
//! [`crate::cluster`] does the grouping; this module only produces the vectors.
//!
//! # Why ONNX rather than a Rust-native model
//!
//! There is no Rust implementation of a competitive speaker-embedding network, and writing one
//! would mean reimplementing and re-validating a research model. ONNX Runtime runs the
//! published weights as published — the same numbers the authors evaluated — and it is the
//! same runtime the reference implementations use, so a disagreement is a bug here rather than
//! an unknown.
//!
//! # Feature-gated
//!
//! Behind `onnx`, off by default. ONNX Runtime is a large native dependency, and a build that
//! only needs [`crate::PauseDiarizer`] should not pay for it. Without the feature the type
//! still exists and every method returns [`DiarizationError::Unavailable`], so callers compile
//! either way.

#[cfg(feature = "onnx")]
use notewise_audio_capture::FbankConfig;
use notewise_audio_capture::FbankExtractor;

use crate::{DiarizationError, Result};

/// Audio shorter than this cannot produce a usable embedding.
///
/// Under about a second the vector is dominated by *what was said* rather than *who said it*,
/// and clustering on it actively degrades the result. Returning an error is better than
/// returning a vector that looks fine and is not.
pub const MIN_EMBEDDING_MS: i64 = 500;

/// A loaded speaker-embedding model.
pub struct SpeakerEmbedder {
    pub(crate) extractor: FbankExtractor,
    /// Learned from the first embedding rather than read out of the model graph.
    ///
    /// Graph introspection differs between ONNX exporters and across `ort` releases; the
    /// output vector does not. Discovering it costs one inference and cannot disagree with
    /// what the model actually produces.
    dimensions: std::sync::atomic::AtomicUsize,

    #[cfg(feature = "onnx")]
    session: std::sync::Mutex<ort::session::Session>,
}

impl std::fmt::Debug for SpeakerEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The ONNX session holds tens of megabytes of weights and prints nothing useful.
        f.debug_struct("SpeakerEmbedder")
            .field("dimensions", &self.dimensions())
            .field("features", self.extractor.config())
            .finish()
    }
}

impl SpeakerEmbedder {
    /// Load a model from an `.onnx` file.
    #[cfg(feature = "onnx")]
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref();

        let builder = ort::session::Session::builder()
            .map_err(|e| DiarizationError::Model(format!("creating a session builder: {e}")))?;

        // One thread per session. The pipeline embeds segments while transcription is already
        // using the machine; oversubscribing makes both slower.
        let mut builder = builder
            .with_intra_threads(1)
            .map_err(|e| DiarizationError::Model(format!("configuring threads: {e}")))?;

        let session = builder
            .commit_from_file(path)
            .map_err(|e| DiarizationError::Model(format!("loading {}: {e}", path.display())))?;

        Ok(Self {
            // Measured, not assumed: with cepstral mean normalisation this model's
            // same-speaker/different-speaker means were 0.380/0.790; without it, 0.285/0.781.
            // The wider margin wins. See `print_the_distance_matrix`.
            extractor: FbankExtractor::new(FbankConfig {
                normalization: notewise_audio_capture::Normalization::None,
                ..Default::default()
            }),
            dimensions: std::sync::atomic::AtomicUsize::new(0),
            session: std::sync::Mutex::new(session),
        })
    }

    #[cfg(not(feature = "onnx"))]
    pub fn load(_path: impl AsRef<std::path::Path>) -> Result<Self> {
        Err(Self::unavailable())
    }

    #[cfg(not(feature = "onnx"))]
    fn unavailable() -> DiarizationError {
        DiarizationError::Unavailable {
            reason: "built without the 'onnx' feature",
        }
    }

    /// Length of the vectors this model produces, or `0` before the first embedding.
    pub fn dimensions(&self) -> usize {
        self.dimensions.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Embed a span of mono audio.
    ///
    /// The returned vector is L2-normalised, so [`crate::cosine_distance`] over it is a dot
    /// product and callers cannot forget to normalise.
    #[cfg(feature = "onnx")]
    pub fn embed(&self, samples: &[f32], sample_rate: u32) -> Result<Vec<f32>> {
        use ort::value::Value;

        let duration_ms = (samples.len() as i64 * 1000) / sample_rate.max(1) as i64;
        if duration_ms < MIN_EMBEDDING_MS {
            return Err(DiarizationError::TooShort {
                got_ms: duration_ms,
                needed_ms: MIN_EMBEDDING_MS,
            });
        }

        let fbank = self.extractor.compute(samples);
        if fbank.is_empty() {
            return Err(DiarizationError::TooShort {
                got_ms: duration_ms,
                needed_ms: MIN_EMBEDDING_MS,
            });
        }

        // [batch, frames, mel_bins]. The batch axis is required even for one segment.
        let input = Value::from_array(([1usize, fbank.frames, fbank.num_bins], fbank.data.clone()))
            .map_err(|e| DiarizationError::Model(format!("building the input tensor: {e}")))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| DiarizationError::Model("the model session was poisoned".into()))?;

        let name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| DiarizationError::Model("the model declares no inputs".into()))?;

        let outputs = session
            .run(ort::inputs![name => input])
            .map_err(|e| DiarizationError::Model(format!("inference failed: {e}")))?;

        let (_, value) = outputs
            .iter()
            .next()
            .ok_or_else(|| DiarizationError::Model("the model returned no output".into()))?;

        let (_, data) = value
            .try_extract_tensor::<f32>()
            .map_err(|e| DiarizationError::Model(format!("reading the embedding: {e}")))?;

        let mut embedding = data.to_vec();
        if embedding.is_empty() {
            return Err(DiarizationError::Model("the embedding was empty".into()));
        }

        crate::cluster::normalize(&mut embedding);
        self.dimensions
            .store(embedding.len(), std::sync::atomic::Ordering::Relaxed);
        Ok(embedding)
    }

    #[cfg(not(feature = "onnx"))]
    pub fn embed(&self, _samples: &[f32], _sample_rate: u32) -> Result<Vec<f32>> {
        Err(Self::unavailable())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "onnx"))]
    #[test]
    fn a_build_without_onnx_fails_loudly_rather_than_returning_a_fake_embedding() {
        let error = SpeakerEmbedder::load("/nonexistent.onnx").expect_err("should fail");
        assert!(matches!(error, DiarizationError::Unavailable { .. }));
        assert!(error.to_string().contains("onnx"), "{error}");
    }

    /// Real inference against a real model.
    ///
    /// `NOTEWISE_SPEAKER_MODEL=/path/to/model.onnx cargo test -p notewise-diarization \
    ///   --features onnx -- --ignored --nocapture`
    #[cfg(feature = "onnx")]
    #[tokio::test]
    #[ignore = "requires a downloaded speaker embedding model"]
    async fn a_real_model_embeds_and_separates_two_voices() {
        let path = std::env::var("NOTEWISE_SPEAKER_MODEL").expect("NOTEWISE_SPEAKER_MODEL");
        let embedder = SpeakerEmbedder::load(&path).expect("load");
        println!("{embedder:?}");

        // Two synthetic "voices": different fundamentals with different harmonic structure.
        // Not real speech, but enough to prove the model produces distinct, stable vectors.
        let voice = |f0: f32, harmonics: &[f32]| -> Vec<f32> {
            (0..16_000 * 3)
                .map(|i| {
                    let t = i as f32 / 16_000.0;
                    harmonics
                        .iter()
                        .enumerate()
                        .map(|(h, amp)| {
                            amp * (2.0 * std::f32::consts::PI * f0 * (h + 1) as f32 * t).sin()
                        })
                        .sum::<f32>()
                        * 0.3
                })
                .collect()
        };

        let low = voice(110.0, &[1.0, 0.5, 0.25, 0.1]);
        let high = voice(210.0, &[1.0, 0.2, 0.6, 0.05]);

        let a = embedder.embed(&low, 16_000).expect("embed low");
        let b = embedder.embed(&high, 16_000).expect("embed high");
        let a2 = embedder.embed(&low, 16_000).expect("embed low again");

        println!("dimensions: {}", a.len());
        assert_eq!(a.len(), embedder.dimensions());

        // Normalised, so a caller cannot forget.
        let norm = a.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "not normalised: {norm}");

        let same = crate::cosine_distance(&a, &a2);
        let different = crate::cosine_distance(&a, &b);
        println!("same voice: {same:.4}   different voices: {different:.4}");

        // Deterministic: the same audio must embed identically, or one speaker drifts between
        // clusters across a meeting.
        assert!(same < 1e-4, "the same audio embedded differently: {same}");
        assert!(
            different > same,
            "two voices were not separated: {different} vs {same}"
        );
    }

    /// Audio too short to identify anyone must be refused, not embedded. A confident vector
    /// derived from half a syllable is worse than no vector.
    #[cfg(feature = "onnx")]
    #[tokio::test]
    #[ignore = "requires a downloaded speaker embedding model"]
    async fn audio_that_is_too_short_is_refused() {
        let path = std::env::var("NOTEWISE_SPEAKER_MODEL").expect("NOTEWISE_SPEAKER_MODEL");
        let embedder = SpeakerEmbedder::load(&path).expect("load");

        let error = embedder
            .embed(&vec![0.1; 1_600], 16_000) // 100 ms
            .expect_err("should refuse");

        assert!(
            matches!(error, DiarizationError::TooShort { .. }),
            "{error}"
        );
    }

    /// What the measurements actually support: on average, the same speaker embeds closer to
    /// itself than to a different speaker.
    ///
    /// Deliberately weaker than "a single threshold separates them", because on the TTS voices
    /// available here it does not — see the note on [`crate::EmbeddingDiarizer`]. This asserts
    /// the model is doing something real without overclaiming what it can do.
    #[cfg(feature = "onnx")]
    #[tokio::test]
    #[ignore = "requires a model and four voice samples"]
    async fn the_same_speaker_is_closer_to_itself_on_average() {
        use notewise_audio_capture::AudioSource;

        let path = std::env::var("NOTEWISE_SPEAKER_MODEL").expect("NOTEWISE_SPEAKER_MODEL");
        let files: Vec<String> = std::env::var("NOTEWISE_MATRIX")
            .expect("NOTEWISE_MATRIX")
            .split(',')
            .map(str::to_string)
            .collect();
        assert_eq!(files.len(), 4, "expected A_s1,A_s2,B_s1,B_s2");

        let embedder = SpeakerEmbedder::load(&path).expect("load");
        let load = |wav: &str| -> Vec<f32> {
            let mut source = notewise_audio_capture::FileSource::open_wav(wav).expect("wav");
            let mut samples = Vec::new();
            while let Some(frame) = source.next_frame().expect("frame") {
                samples.extend_from_slice(&frame.to_transcription_format().samples);
            }
            samples
        };

        let e: Vec<Vec<f32>> = files
            .iter()
            .map(|f| embedder.embed(&load(f), 16_000).expect("embed"))
            .collect();

        let same =
            (crate::cosine_distance(&e[0], &e[1]) + crate::cosine_distance(&e[2], &e[3])) / 2.0;
        let different = (crate::cosine_distance(&e[0], &e[2])
            + crate::cosine_distance(&e[0], &e[3])
            + crate::cosine_distance(&e[1], &e[2])
            + crate::cosine_distance(&e[1], &e[3]))
            / 4.0;

        println!("same-speaker mean {same:.3}, different-speaker mean {different:.3}");
        assert!(
            same < different,
            "the embedding is not separating speakers at all: {same:.3} vs {different:.3}"
        );

        // The classes must not merely differ on average — they must not overlap, or no single
        // threshold can separate them and clustering is guessing.
        let worst_same =
            crate::cosine_distance(&e[0], &e[1]).max(crate::cosine_distance(&e[2], &e[3]));
        let best_different = [
            crate::cosine_distance(&e[0], &e[2]),
            crate::cosine_distance(&e[0], &e[3]),
            crate::cosine_distance(&e[1], &e[2]),
            crate::cosine_distance(&e[1], &e[3]),
        ]
        .into_iter()
        .fold(f32::MAX, f32::min);

        println!("worst same {worst_same:.3}, best different {best_different:.3}");
        assert!(
            worst_same < best_different,
            "the classes overlap: same-speaker reached {worst_same:.3} while different \
             speakers came as close as {best_different:.3}"
        );

        let threshold = crate::ClusterConfig::default().threshold;
        assert!(
            (worst_same..best_different).contains(&threshold),
            "the configured threshold {threshold:.3} is outside the measured gap \
             {worst_same:.3}..{best_different:.3}"
        );

        // End to end: the clustering must actually recover two speakers.
        let labels = crate::cluster::cluster(&e, crate::ClusterConfig::default());
        println!("clustered as {labels:?}");
        assert_eq!(labels[0], labels[1], "speaker A split apart");
        assert_eq!(labels[2], labels[3], "speaker B split apart");
        assert_ne!(labels[0], labels[2], "the two speakers merged");
    }

    /// Diagnostic: the full distance matrix, with and without cepstral mean normalisation.
    ///
    /// `NOTEWISE_MATRIX=A_s1.wav,A_s2.wav,B_s1.wav,B_s2.wav` — two speakers, two utterances
    /// each, in that order.
    #[cfg(feature = "onnx")]
    #[tokio::test]
    #[ignore = "diagnostic; requires a model and four voice samples"]
    async fn print_the_distance_matrix() {
        use notewise_audio_capture::AudioSource;

        let path = std::env::var("NOTEWISE_SPEAKER_MODEL").expect("NOTEWISE_SPEAKER_MODEL");
        let files: Vec<String> = std::env::var("NOTEWISE_MATRIX")
            .expect("NOTEWISE_MATRIX")
            .split(',')
            .map(str::to_string)
            .collect();

        let load = |wav: &str| -> Vec<f32> {
            let mut source = notewise_audio_capture::FileSource::open_wav(wav).expect("wav");
            let mut samples = Vec::new();
            while let Some(frame) = source.next_frame().expect("frame") {
                samples.extend_from_slice(&frame.to_transcription_format().samples);
            }
            samples
        };
        let audio: Vec<Vec<f32>> = files.iter().map(|f| load(f)).collect();
        let labels = ["A_s1", "A_s2", "B_s1", "B_s2"];

        for normalization in [
            notewise_audio_capture::Normalization::Mean,
            notewise_audio_capture::Normalization::None,
        ] {
            let mut embedder = SpeakerEmbedder::load(&path).expect("load");
            embedder.extractor = FbankExtractor::new(FbankConfig {
                normalization,
                ..Default::default()
            });

            let embeddings: Vec<Vec<f32>> = audio
                .iter()
                .map(|a| embedder.embed(a, 16_000).expect("embed"))
                .collect();

            println!("\n=== normalization = {normalization:?} ===");
            print!("{:>8}", "");
            for l in &labels {
                print!("{l:>8}");
            }
            println!();
            for (i, a) in embeddings.iter().enumerate() {
                print!("{:>8}", labels[i]);
                for b in &embeddings {
                    print!("{:>8.3}", crate::cosine_distance(a, b));
                }
                println!();
            }
            let same_a = crate::cosine_distance(&embeddings[0], &embeddings[1]);
            let same_b = crate::cosine_distance(&embeddings[2], &embeddings[3]);
            let diff = crate::cosine_distance(&embeddings[0], &embeddings[2]);
            println!(
                "SUMMARY same-speaker avg {:.3}  different-speaker {:.3}  separated: {}",
                (same_a + same_b) / 2.0,
                diff,
                if (same_a + same_b) / 2.0 < diff {
                    "YES"
                } else {
                    "NO"
                }
            );
        }
    }
}
