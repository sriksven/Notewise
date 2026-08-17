//! Model registry and on-disk store.
//!
//! Real and tested: which models exist, where they live, whether one is present, and
//! integrity verification. The download itself is real too — the network call is the only
//! part a test cannot exercise, and it is marked `#[ignore]` rather than mocked away.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Result, TranscriptionError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSize {
    Tiny,
    Base,
    Small,
    /// Large-class accuracy with a pruned decoder. Not a size so much as a different trade —
    /// see [`ModelSize::tradeoff`].
    Turbo,
    Medium,
    Large,
}

impl ModelSize {
    pub fn as_str(&self) -> &'static str {
        match self {
            ModelSize::Tiny => "tiny",
            ModelSize::Base => "base",
            ModelSize::Small => "small",
            ModelSize::Turbo => "turbo",
            ModelSize::Medium => "medium",
            ModelSize::Large => "large",
        }
    }

    /// What picking this size buys and costs, in a sentence.
    ///
    /// These names come from Whisper and mean nothing to anyone who has not read its paper.
    /// A menu of `tiny.en / base / small.en / medium` with only megabytes beside each is not a
    /// choice a user can make — it is a quiz. Every entry has to say what it is for.
    ///
    /// Deliberately qualitative. Real throughput depends on the machine, the audio and whether
    /// Metal is compiled in, and a fabricated "16x realtime" would be a number we cannot stand
    /// behind on someone else's laptop.
    pub fn tradeoff(&self) -> &'static str {
        match self {
            ModelSize::Tiny => {
                "Quickest and least accurate. Usable for a rough record — expect names, \
                 acronyms and technical terms to come out wrong."
            }
            ModelSize::Base => {
                "The default, and the right starting point. Handles clear speech on a decent \
                 microphone well, and keeps up with a live meeting comfortably."
            }
            ModelSize::Small => {
                "Clearly better on accents, crosstalk and jargon. Several times slower than \
                 base, and still fast enough to record live on a modern machine."
            }
            ModelSize::Turbo => {
                "The best choice for real meetings. Large-v3's accuracy with most of the \
                 decoder removed, so it handles accents, crosstalk and a laptop microphone far \
                 better than base or small while staying fast enough to keep up live."
            }
            ModelSize::Medium => {
                "Better again, and heavy. Best kept for importing a recording after the fact \
                 rather than transcribing as people speak. Turbo is usually the better pick at \
                 this size."
            }
            ModelSize::Large => {
                "The most accurate available. Wants a lot of memory and is by a wide margin \
                 the slowest — worth it for a recording that matters, not for everyday use."
            }
        }
    }
}

/// One downloadable model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub size: ModelSize,
    pub url: String,
    /// Expected file size. Cheap integrity check that catches the common failure —
    /// a truncated download from an interrupted connection.
    pub bytes: u64,
    /// Whether this model handles languages other than English.
    pub multilingual: bool,
}

impl ModelInfo {
    pub fn filename(&self) -> String {
        format!("ggml-{}.bin", self.name)
    }

    /// This model as a downloadable file.
    pub fn artifact(&self) -> Artifact {
        Artifact {
            name: self.name.clone(),
            filename: self.filename(),
            url: self.url.clone(),
            bytes: self.bytes,
        }
    }

    /// Rough working-set estimate, for warning a user before they pick a model their
    /// machine cannot run.
    pub fn approx_ram_mb(&self) -> u64 {
        // Runtime footprint runs well above the file size; ~1.6x is a workable estimate.
        (self.bytes / 1_000_000) * 8 / 5
    }

    /// What the missing or present `.en` suffix means for this entry.
    ///
    /// The suffix is the least guessable part of the naming. `base` and `base.en` are the same
    /// size and differ only in what they were trained on, and the English-only one is the
    /// better choice for an English meeting — which is the opposite of what "fewer languages"
    /// suggests.
    pub fn language_note(&self) -> &'static str {
        if self.multilingual {
            "Understands languages other than English."
        } else {
            "English only, and more accurate on English than the same-sized model that is not."
        }
    }
}

/// The known models.
#[derive(Debug, Clone, Copy)]
pub struct ModelRegistry;

impl ModelRegistry {
    const BASE_URL: &'static str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

    /// Every model, smallest first.
    pub fn all() -> Vec<ModelInfo> {
        // Real published artifact sizes, read from the CDN's Content-Length. These were
        // previously rounded approximations, which rejected every legitimately downloaded
        // model — the check is only useful if the numbers are exact.
        let entries = [
            ("tiny.en", ModelSize::Tiny, 77_704_715u64, false),
            ("tiny", ModelSize::Tiny, 77_691_713, true),
            ("base.en", ModelSize::Base, 147_964_211, false),
            ("base", ModelSize::Base, 147_951_465, true),
            ("small.en", ModelSize::Small, 487_614_201, false),
            ("small", ModelSize::Small, 487_601_967, true),
            // Quantised turbo. The reason this list exists: at roughly the download size of
            // `small`, it is a large-class model. Anyone getting poor transcripts from `base`
            // on a real microphone wants this, and until now the catalogue did not offer it.
            ("large-v3-turbo-q5_0", ModelSize::Turbo, 574_041_195, true),
            ("medium.en", ModelSize::Medium, 1_533_774_781, false),
            ("medium", ModelSize::Medium, 1_533_763_059, true),
            ("large-v3-turbo", ModelSize::Turbo, 1_624_555_275, true),
            ("large-v3", ModelSize::Large, 3_095_033_483, true),
        ];

        entries
            .into_iter()
            .map(|(name, size, bytes, multilingual)| ModelInfo {
                name: name.to_string(),
                size,
                url: format!("{}/ggml-{name}.bin", Self::BASE_URL),
                bytes,
                multilingual,
            })
            .collect()
    }

    pub fn get(name: &str) -> Result<ModelInfo> {
        Self::all()
            .into_iter()
            .find(|m| m.name == name)
            .ok_or_else(|| TranscriptionError::UnknownModel(name.to_string()))
    }

    /// The default model.
    ///
    /// `base.en` balances accuracy against a download a user will actually wait for. Larger
    /// models are meaningfully better but start at half a gigabyte.
    pub fn default_model() -> ModelInfo {
        Self::get("base.en").expect("base.en is always in the registry")
    }

    /// Models that fit comfortably in the given RAM budget.
    pub fn fitting_in_ram(available_mb: u64) -> Vec<ModelInfo> {
        Self::all()
            .into_iter()
            .filter(|m| m.approx_ram_mb() <= available_mb)
            .collect()
    }
}

/// A file to fetch once and keep on disk.
///
/// Extracted from [`ModelInfo`] so the same resumable, size-verified download can serve models
/// this crate knows nothing about. A speaker-embedding network belongs to `diarization`, and
/// giving it its own downloader would mean two implementations of resume-and-verify drifting
/// apart — with the interesting failures (a server that ignores `Range`, a truncated file that
/// looks installed) fixed in only one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// Used only in errors and logs.
    pub name: String,
    pub filename: String,
    pub url: String,
    /// Exact expected size. The integrity check, and the reason a resume knows what "partial" is.
    pub bytes: u64,
}

/// How far a download has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    pub downloaded_bytes: u64,
    /// Total size. Taken from `Content-Length` when the server sends one, otherwise the
    /// registry's recorded size — a progress bar with no end is worse than a slightly wrong one.
    pub total_bytes: u64,
    pub done: bool,
}

impl DownloadProgress {
    fn complete(total: u64) -> Self {
        Self {
            downloaded_bytes: total,
            total_bytes: total,
            done: true,
        }
    }

    /// Fraction complete, in `0.0..=1.0`.
    pub fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.downloaded_bytes as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
    }

    pub fn percent(&self) -> u8 {
        (self.fraction() * 100.0).round() as u8
    }
}

/// Models on disk.
#[derive(Debug, Clone)]
pub struct ModelStore {
    dir: PathBuf,
}

impl ModelStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path_for(&self, model: &ModelInfo) -> PathBuf {
        self.artifact_path(&model.artifact())
    }

    pub fn artifact_path(&self, artifact: &Artifact) -> PathBuf {
        self.dir.join(&artifact.filename)
    }

    /// Whether the model is present and the right size.
    ///
    /// Size is checked, not just existence: a download interrupted at 90% leaves a file that
    /// exists and fails at load time with a far less obvious error.
    pub fn is_available(&self, model: &ModelInfo) -> bool {
        self.has_artifact(&model.artifact())
    }

    pub fn has_artifact(&self, artifact: &Artifact) -> bool {
        std::fs::metadata(self.artifact_path(artifact))
            .map(|m| m.len() == artifact.bytes)
            .unwrap_or(false)
    }

    /// Verify a downloaded model, returning a specific error if it is wrong.
    pub fn verify(&self, model: &ModelInfo) -> Result<()> {
        self.verify_artifact(&model.artifact())
    }

    pub fn verify_artifact(&self, artifact: &Artifact) -> Result<()> {
        let path = self.artifact_path(artifact);

        let metadata =
            std::fs::metadata(&path).map_err(|_| TranscriptionError::ModelNotDownloaded {
                name: artifact.name.clone(),
            })?;

        if metadata.len() != artifact.bytes {
            return Err(TranscriptionError::ModelCorrupt {
                name: artifact.name.clone(),
                expected: artifact.bytes,
                actual: metadata.len(),
            });
        }

        Ok(())
    }

    /// Models already downloaded.
    pub fn installed(&self) -> Vec<ModelInfo> {
        ModelRegistry::all()
            .into_iter()
            .filter(|m| self.is_available(m))
            .collect()
    }

    /// Delete a downloaded model.
    pub fn remove(&self, model: &ModelInfo) -> Result<()> {
        self.remove_artifact(&model.artifact())
    }

    pub fn remove_artifact(&self, artifact: &Artifact) -> Result<()> {
        let path = self.artifact_path(artifact);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    /// Download a model.
    ///
    /// Writes to a temporary file and renames on success, so an interrupted download never
    /// leaves a partial file that looks installed.
    pub async fn download(&self, model: &ModelInfo) -> Result<PathBuf> {
        self.download_with_progress(model, |_| {}).await
    }

    /// Download a model, reporting progress as it goes.
    ///
    /// # Streamed, not buffered
    ///
    /// Chunks are written to disk as they arrive rather than collected into a `Vec` first.
    /// `large-v3` is 3.1 GB; buffering it needs 3.1 GB of resident memory on a machine that
    /// may only have 8, and the user has no idea why the app died.
    ///
    /// # Resumable
    ///
    /// A partial file is resumed with a `Range` request. A multi-gigabyte download over hotel
    /// wifi will be interrupted, and restarting from zero every time means it may never finish.
    /// If the server ignores the range and replies `200`, the partial file is discarded and the
    /// download restarts — appending to it would silently corrupt the model, and a corrupt
    /// model is far worse than a slow one.
    ///
    /// # Progress
    ///
    /// `on_progress` is called as bytes arrive, and once more on completion. It must be cheap:
    /// it runs on the download path, so blocking in it throttles the transfer.
    pub async fn download_with_progress(
        &self,
        model: &ModelInfo,
        on_progress: impl FnMut(DownloadProgress),
    ) -> Result<PathBuf> {
        self.fetch(&model.artifact(), on_progress).await
    }

    /// Download any artifact, with the same resume, verification and progress reporting.
    pub async fn fetch(
        &self,
        model: &Artifact,
        mut on_progress: impl FnMut(DownloadProgress),
    ) -> Result<PathBuf> {
        use futures_util::StreamExt;
        use std::io::Write;

        let final_path = self.artifact_path(model);
        if self.has_artifact(model) {
            on_progress(DownloadProgress::complete(model.bytes));
            return Ok(final_path);
        }

        std::fs::create_dir_all(&self.dir)?;
        let temp_path = final_path.with_extension("partial");

        // Resume only from a partial that is plausibly this model. Anything at or past the
        // expected size is not a resumable prefix — it is junk from an interrupted write or a
        // different model, and continuing from it would produce a file that fails `verify`
        // after another multi-gigabyte transfer.
        let resume_from = match std::fs::metadata(&temp_path) {
            Ok(meta) if meta.len() > 0 && meta.len() < model.bytes => meta.len(),
            Ok(_) => {
                let _ = std::fs::remove_file(&temp_path);
                0
            }
            Err(_) => 0,
        };

        let mut request = reqwest::Client::new().get(&model.url);
        if resume_from > 0 {
            tracing::info!(model = %model.name, resume_from, "resuming download");
            request = request.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
        }

        let response = request
            .send()
            .await
            .map_err(|e| TranscriptionError::Download(e.to_string()))?;

        if !response.status().is_success() {
            return Err(TranscriptionError::Download(format!(
                "{} returned {}",
                model.url,
                response.status()
            )));
        }

        // 206 means the range was honoured. A 200 to a range request means it was not, so the
        // body is the whole file and anything already on disk must go.
        let resumed = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        let mut downloaded = if resumed { resume_from } else { 0 };

        if resume_from > 0 && !resumed {
            tracing::warn!(
                model = %model.name,
                "the server ignored the range request; restarting from zero"
            );
        }

        let total = response
            .content_length()
            .map(|len| len + downloaded)
            .unwrap_or(model.bytes);

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(&temp_path)?;

        on_progress(DownloadProgress {
            downloaded_bytes: downloaded,
            total_bytes: total,
            done: false,
        });

        let mut stream = response.bytes_stream();
        // Report on a byte interval rather than per chunk: chunks are ~8 KB, and a callback
        // that pushes an SSE frame per chunk would send hundreds of thousands of them.
        const REPORT_EVERY: u64 = 1024 * 1024;
        let mut next_report = downloaded + REPORT_EVERY;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| TranscriptionError::Download(e.to_string()))?;
            file.write_all(&chunk)?;
            downloaded += chunk.len() as u64;

            if downloaded >= next_report {
                next_report = downloaded + REPORT_EVERY;
                on_progress(DownloadProgress {
                    downloaded_bytes: downloaded,
                    total_bytes: total,
                    done: false,
                });
            }
        }

        file.flush()?;
        drop(file);

        std::fs::rename(&temp_path, &final_path)?;

        // Verified after the rename so a corrupt artifact is reported rather than trusted.
        // A failed verify removes the file: leaving it would make `is_available` true and
        // every later run would load a broken model instead of re-downloading.
        if let Err(e) = self.verify_artifact(model) {
            let _ = std::fs::remove_file(&final_path);
            return Err(e);
        }

        on_progress(DownloadProgress::complete(total));
        Ok(final_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("notewise-models-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn the_registry_is_not_empty_and_has_unique_names() {
        let models = ModelRegistry::all();
        assert!(models.len() >= 8);

        let mut names = std::collections::HashSet::new();
        for model in &models {
            assert!(names.insert(&model.name), "duplicate model {}", model.name);
        }
    }

    #[test]
    fn every_model_has_a_plausible_url_and_size() {
        for model in ModelRegistry::all() {
            assert!(model.url.starts_with("https://"), "{}", model.url);
            assert!(model.url.ends_with(&model.filename()), "{}", model.url);
            assert!(model.bytes > 1_000_000, "{} looks too small", model.name);
        }
    }

    #[test]
    fn the_default_model_is_english_and_mid_sized() {
        let model = ModelRegistry::default_model();
        assert_eq!(model.name, "base.en");
        assert!(!model.multilingual);
    }

    #[test]
    fn unknown_models_are_reported_not_guessed() {
        assert!(matches!(
            ModelRegistry::get("gpt-9").unwrap_err(),
            TranscriptionError::UnknownModel(_)
        ));
    }

    #[test]
    fn ram_estimates_order_with_model_size() {
        let tiny = ModelRegistry::get("tiny.en").unwrap();
        let large = ModelRegistry::get("large-v3").unwrap();

        assert!(tiny.approx_ram_mb() < large.approx_ram_mb());
        assert!(tiny.approx_ram_mb() > 0);
    }

    #[test]
    fn ram_filtering_excludes_models_that_will_not_fit() {
        let small_machine = ModelRegistry::fitting_in_ram(200);

        assert!(small_machine.iter().any(|m| m.size == ModelSize::Tiny));
        assert!(
            !small_machine.iter().any(|m| m.size == ModelSize::Large),
            "a 3GB model must not be offered on a 200MB budget"
        );
    }

    #[test]
    fn a_missing_model_is_not_available() {
        let store = ModelStore::new(temp_dir("missing"));
        assert!(!store.is_available(&ModelRegistry::default_model()));
        assert!(store.installed().is_empty());
    }

    #[test]
    fn verifying_a_missing_model_says_it_is_not_downloaded() {
        let store = ModelStore::new(temp_dir("verify-missing"));
        assert!(matches!(
            store.verify(&ModelRegistry::default_model()).unwrap_err(),
            TranscriptionError::ModelNotDownloaded { .. }
        ));
    }

    #[test]
    fn a_truncated_download_is_detected_rather_than_loaded() {
        // The realistic failure: a connection dropped part-way leaves a plausible-looking
        // file that fails much later with a far more confusing error.
        let dir = temp_dir("truncated");
        let store = ModelStore::new(&dir);
        let model = ModelRegistry::default_model();

        std::fs::write(store.path_for(&model), b"not the whole model").unwrap();

        assert!(!store.is_available(&model), "size mismatch must not pass");
        let err = store.verify(&model).unwrap_err();
        assert!(
            matches!(err, TranscriptionError::ModelCorrupt { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn a_correctly_sized_file_verifies() {
        let dir = temp_dir("valid");
        let store = ModelStore::new(&dir);

        // Use a small synthetic entry rather than writing 148MB in a test.
        let model = ModelInfo {
            name: "test-model".into(),
            size: ModelSize::Tiny,
            url: "https://example.invalid/ggml-test-model.bin".into(),
            bytes: 12,
            multilingual: false,
        };
        std::fs::write(store.path_for(&model), b"123456789012").unwrap();

        assert!(store.is_available(&model));
        assert!(store.verify(&model).is_ok());
    }

    #[test]
    fn removing_a_model_is_idempotent() {
        let store = ModelStore::new(temp_dir("remove"));
        let model = ModelRegistry::default_model();

        assert!(
            store.remove(&model).is_ok(),
            "removing what is absent is fine"
        );
        assert!(store.remove(&model).is_ok());
    }

    #[test]
    fn paths_are_namespaced_by_model_name() {
        let store = ModelStore::new("/models");
        let base = ModelRegistry::get("base.en").unwrap();
        let tiny = ModelRegistry::get("tiny.en").unwrap();

        assert_ne!(store.path_for(&base), store.path_for(&tiny));
        assert!(store.path_for(&base).ends_with("ggml-base.en.bin"));
    }

    #[tokio::test]
    async fn downloading_a_present_model_short_circuits() {
        let dir = temp_dir("short-circuit");
        let store = ModelStore::new(&dir);
        let model = ModelInfo {
            name: "cached".into(),
            size: ModelSize::Tiny,
            // Unreachable on purpose: reaching the network here would be the bug.
            url: "https://example.invalid/ggml-cached.bin".into(),
            bytes: 4,
            multilingual: false,
        };
        std::fs::write(store.path_for(&model), b"abcd").unwrap();

        assert!(store.download(&model).await.is_ok());
    }

    #[tokio::test]
    #[ignore = "downloads ~148MB over the network"]
    async fn downloads_the_default_model() {
        let dir = temp_dir("real-download");
        let store = ModelStore::new(&dir);
        let model = ModelRegistry::default_model();

        let path = store.download(&model).await.expect("download");
        assert!(path.exists());
        assert!(store.verify(&model).is_ok());
    }

    // ------------------------------------------------------------------ download progress

    #[test]
    fn progress_fraction_and_percent_are_bounded() {
        let half = DownloadProgress {
            downloaded_bytes: 50,
            total_bytes: 100,
            done: false,
        };
        assert!((half.fraction() - 0.5).abs() < f32::EPSILON);
        assert_eq!(half.percent(), 50);

        // Content-Length can under-report; the bar must not exceed full.
        let over = DownloadProgress {
            downloaded_bytes: 150,
            total_bytes: 100,
            done: false,
        };
        assert_eq!(over.fraction(), 1.0);
        assert_eq!(over.percent(), 100);
    }

    /// An unknown total must not divide by zero or report NaN into a progress bar.
    #[test]
    fn a_zero_total_reports_no_progress_rather_than_nan() {
        let unknown = DownloadProgress {
            downloaded_bytes: 10,
            total_bytes: 0,
            done: false,
        };
        assert_eq!(unknown.fraction(), 0.0);
        assert_eq!(unknown.percent(), 0);
    }

    #[test]
    fn a_completed_download_reports_full() {
        let done = DownloadProgress::complete(1_000);
        assert!(done.done);
        assert_eq!(done.percent(), 100);
        assert_eq!(done.downloaded_bytes, done.total_bytes);
    }

    /// An already-installed model must still report completion, or a UI that waits for a
    /// terminal event would spin forever on a no-op download.
    #[tokio::test]
    async fn downloading_an_installed_model_reports_completion_immediately() {
        let dir = temp_dir("already-installed");
        let store = ModelStore::new(&dir);
        let model = ModelRegistry::get("tiny.en").expect("tiny.en");

        std::fs::write(store.path_for(&model), vec![0u8; model.bytes as usize])
            .expect("fake model");

        let mut reports = Vec::new();
        store
            .download_with_progress(&model, |p| reports.push(p))
            .await
            .expect("download");

        assert_eq!(reports.len(), 1);
        assert!(reports[0].done);
    }

    /// A `.partial` at or past the expected size is not a resumable prefix — it is junk from
    /// an interrupted write or a different model. Resuming from it would append onto garbage
    /// and produce a file that fails `verify` after another multi-gigabyte transfer.
    #[tokio::test]
    async fn an_oversized_partial_file_is_discarded_rather_than_resumed() {
        let dir = temp_dir("bad-partial");
        let store = ModelStore::new(&dir);
        let model = ModelRegistry::get("tiny.en").expect("tiny.en");

        let partial = store.path_for(&model).with_extension("partial");
        std::fs::write(&partial, vec![0u8; model.bytes as usize + 1]).expect("partial");

        // No network here: the call fails at the request, but the junk must already be gone.
        let _ = store
            .download_with_progress(&model, |_| {})
            .await
            .map_err(|_| ());

        assert!(
            !partial.exists() || std::fs::metadata(&partial).unwrap().len() < model.bytes,
            "an unusable partial was kept"
        );
    }

    /// The real thing, over the network. Ignored because it moves ~78 MB.
    ///
    /// Asserts what a unit test cannot: that progress actually arrives during the transfer
    /// rather than once at the end, which is the entire point of streaming.
    #[tokio::test]
    #[ignore = "downloads ~78MB over the network"]
    async fn a_real_download_streams_progress_and_verifies() {
        let dir = temp_dir("real-stream");
        let store = ModelStore::new(&dir);
        let model = ModelRegistry::get("tiny.en").expect("tiny.en");

        let reports = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&reports);

        let path = store
            .download_with_progress(&model, move |p| sink.lock().unwrap().push(p))
            .await
            .expect("download");

        let reports = reports.lock().unwrap();
        println!("{} progress reports for {}", reports.len(), model.name);

        assert!(
            reports.len() > 5,
            "expected streamed progress, got {} report(s) — this is buffering, not streaming",
            reports.len()
        );
        assert!(reports.last().expect("final").done);

        // Monotonic: a bar that goes backwards is a bug the user can see.
        for pair in reports.windows(2) {
            assert!(
                pair[1].downloaded_bytes >= pair[0].downloaded_bytes,
                "progress went backwards: {:?} -> {:?}",
                pair[0],
                pair[1]
            );
        }

        assert!(path.exists());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), model.bytes);
        store.verify(&model).expect("verify");
    }
}
