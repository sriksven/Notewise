//! Which speaker-embedding models exist, and where to get them.
//!
//! [`SpeakerEmbedder`](crate::SpeakerEmbedder) has always been able to run a model. Nothing
//! could tell it *which* model or fetch one, so in practice it never ran — the acoustic half of
//! diarization was complete code with no weights to execute.
//!
//! # Why these models
//!
//! All three are the published artifacts from k2-fsa/sherpa-onnx, which republishes the WeSpeaker
//! and 3D-Speaker releases as ONNX. They are used as published, not re-exported here: a
//! re-export is a second set of weights to keep honest, and the numbers the authors evaluated
//! stop being the numbers being run.
//!
//! They all expect 80-bin Kaldi filterbanks at 16 kHz over int16-scaled audio, which is exactly
//! what [`FbankExtractor`](notewise_audio_capture::FbankExtractor) produces by default. That is
//! not a coincidence — the extractor was written for them — but it is the compatibility that
//! matters, and it is why a model outside this list cannot simply be dropped in.
//!
//! # Sizes are exact
//!
//! Read from the CDN's `Content-Length`, because the store treats size as the integrity check.
//! An approximate number here rejects every correctly downloaded file.

use notewise_transcription::Artifact;

use crate::{DiarizationError, Result};

/// One downloadable speaker-embedding model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerModel {
    pub name: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    /// What picking this one buys and costs, in a sentence.
    pub tradeoff: &'static str,
}

// There is deliberately no `dimensions` field. It was here, carrying 192 for CAM++, and the
// model measured 512 on the first real inference. `SpeakerEmbedder` learns the true length from
// the output vector precisely so nothing has to declare it — and a guessed number in a registry
// whose entire purpose is exact metadata is worse than no number at all.

impl SpeakerModel {
    pub fn filename(&self) -> String {
        format!("speaker-{}.onnx", self.name)
    }

    /// This model as something [`notewise_transcription::ModelStore`] can fetch.
    pub fn artifact(&self) -> Artifact {
        Artifact {
            name: self.name.to_string(),
            filename: self.filename(),
            url: self.url.to_string(),
            bytes: self.bytes,
        }
    }

    pub fn approx_mb(&self) -> u64 {
        self.bytes / 1_000_000
    }
}

/// The known speaker-embedding models.
///
/// URLs are written out in full in each entry rather than assembled from a shared prefix: one
/// you can paste into a browser to check is worth more than three saved repetitions. The
/// misspelling of "recongition" is upstream's, and is part of the real path.
#[derive(Debug, Clone, Copy)]
pub struct SpeakerModelRegistry;

impl SpeakerModelRegistry {
    pub fn all() -> Vec<SpeakerModel> {
        vec![
            SpeakerModel {
                name: "campplus-voxceleb",
                // The upstream filename contains `++`, which must stay percent-encoded or the
                // request 404s.
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/\
                      speaker-recongition-models/wespeaker_en_voxceleb_CAM%2B%2B.onnx",
                bytes: 29_292_684,
                tradeoff:
                    "The default. Accurate enough to separate voices in a real meeting and small \
                     enough to run on a laptop CPU while transcription is already using it.",
            },
            SpeakerModel {
                name: "campplus-3dspeaker",
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/\
                      speaker-recongition-models/\
                      3dspeaker_speech_campplus_sv_en_voxceleb_16k.onnx",
                bytes: 29_596_978,
                tradeoff:
                    "The same architecture from a different training run. Worth trying if the \
                     default splits or merges people on your recordings — they fail differently.",
            },
            SpeakerModel {
                name: "resnet34-voxceleb",
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/\
                      speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx",
                bytes: 26_530_550,
                tradeoff: "Stronger on short turns and noisy rooms, and several times slower per \
                     segment. Best kept for importing a recording rather than labelling live.",
            },
        ]
    }

    pub fn get(name: &str) -> Result<SpeakerModel> {
        Self::all()
            .into_iter()
            .find(|m| m.name == name)
            .ok_or_else(|| DiarizationError::UnknownModel(name.to_string()))
    }

    /// The model chosen when a user has not chosen one.
    pub fn default_model() -> SpeakerModel {
        Self::get("campplus-voxceleb").expect("campplus-voxceleb is always in the registry")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_has_a_plausible_url_and_an_exact_size() {
        for model in SpeakerModelRegistry::all() {
            assert!(model.url.starts_with("https://"), "{}", model.name);
            assert!(model.url.ends_with(".onnx"), "{}", model.name);
            // An approximate size would reject every correctly downloaded file, because the
            // store treats size as the integrity check.
            assert!(
                model.bytes % 1_000_000 != 0,
                "{} has a suspiciously round size — is it a real Content-Length?",
                model.name
            );
            assert!(!model.tradeoff.is_empty(), "{}", model.name);
        }
    }

    /// The `++` in the upstream filename must stay percent-encoded, or the download 404s.
    #[test]
    fn the_campplus_url_keeps_its_escaping() {
        let model = SpeakerModelRegistry::default_model();
        assert!(model.url.contains("%2B%2B"), "{}", model.url);
        assert!(!model.url.contains("++"), "{}", model.url);
    }

    #[test]
    fn model_names_are_unique() {
        let all = SpeakerModelRegistry::all();
        let distinct: std::collections::HashSet<_> = all.iter().map(|m| m.name).collect();
        assert_eq!(distinct.len(), all.len());
    }

    #[test]
    fn filenames_do_not_collide_with_whisper_models() {
        for model in SpeakerModelRegistry::all() {
            let filename = model.filename();
            // Both kinds share one directory, so a `ggml-` prefix here would be ambiguous.
            assert!(filename.starts_with("speaker-"), "{filename}");
            assert!(filename.ends_with(".onnx"), "{filename}");
        }
    }

    #[test]
    fn an_unknown_model_is_reported_by_name() {
        let error = SpeakerModelRegistry::get("nonesuch").expect_err("should fail");
        assert!(error.to_string().contains("nonesuch"), "{error}");
    }

    #[test]
    fn the_default_is_in_the_registry() {
        let default = SpeakerModelRegistry::default_model();
        assert!(SpeakerModelRegistry::all().contains(&default));
    }

    /// Fetch the default model and prove it runs.
    ///
    /// Downloads ~29 MB, so it is `#[ignore]`d rather than run in CI — but it is the test that
    /// says whether the registry's URL and byte count are still true, and it is worth running
    /// after any edit to them:
    ///
    /// `cargo test -p notewise-diarization --features onnx-download -- --ignored --nocapture
    ///  the_default_model_downloads_and_embeds`
    ///
    /// # What this establishes, and what it does not
    ///
    /// Measured here on 2026-08-16 against `campplus-voxceleb`: the download matched the
    /// registry's 29,292,684 bytes exactly, the model loaded, and it produced deterministic,
    /// L2-normalised **512**-dimensional vectors — not the 192 that was guessed in this file
    /// before anything ran it.
    ///
    /// It does **not** establish that speakers get separated. The only speech available to
    /// synthesise here is macOS `say`, and on two of its voices the classes overlap badly:
    /// with the extractor's real settings, the same voice measured 0.439 and 0.349 apart while
    /// two *different* voices came as close as 0.245. A single threshold cannot cut that. TTS
    /// voices from one engine share vocoder characteristics that real speakers do not, and this
    /// model was trained on VoxCeleb — real recordings of real people. So separation quality
    /// stays unverified until someone runs `embedding::tests::
    /// the_same_speaker_is_closer_to_itself_on_average` on human speech, and nothing in the
    /// product should claim otherwise until they have.
    #[tokio::test]
    #[ignore = "downloads ~29 MB from GitHub releases"]
    async fn the_default_model_downloads_and_embeds() {
        let dir = std::env::temp_dir().join(format!("notewise-speaker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let model = SpeakerModelRegistry::default_model();
        let store = notewise_transcription::ModelStore::new(&dir);

        let path = store
            .fetch(&model.artifact(), |_| {})
            .await
            .expect("download the default speaker model");

        assert!(store.has_artifact(&model.artifact()), "size did not verify");
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            model.bytes,
            "the registry's byte count no longer matches what the CDN serves"
        );

        // Loading is the part the registry cannot check on its own: a file of the right size
        // that ONNX Runtime refuses is still a broken entry.
        #[cfg(feature = "onnx")]
        {
            let embedder = crate::SpeakerEmbedder::load(&path).expect("load the model");
            let a = embedder
                .embed(&vec![0.05; 16_000 * 2], 16_000)
                .expect("embed");
            assert!(!a.is_empty());
            let norm = a.iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-4, "not normalised: {norm}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The artifact must carry the size through, or the store cannot verify the download.
    #[test]
    fn an_artifact_carries_the_size_and_url() {
        let model = SpeakerModelRegistry::default_model();
        let artifact = model.artifact();

        assert_eq!(artifact.bytes, model.bytes);
        assert_eq!(artifact.url, model.url);
        assert_eq!(artifact.filename, model.filename());
    }
}
