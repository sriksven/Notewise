//! Model downloads, tracked and streamable.
//!
//! # Why this is not just a blocking POST
//!
//! `large-v3` is 3.1 GB. A request that holds the connection open until it finishes will be
//! killed by a proxy, a laptop sleeping, or a user who assumes the app has hung — and there is
//! no way to tell any of those apart from a download that is simply still going. Worse, a
//! retried POST would start a *second* download of the same file.
//!
//! So a download is a resource with a lifetime: `POST` starts one and returns immediately,
//! `GET` streams its progress, and starting one that is already running returns the running
//! one rather than duplicating it.

use std::collections::HashMap;
use std::sync::Arc;

use notewise_transcription::{Artifact, ModelInfo, ModelStore};
use serde::Serialize;
use tokio::sync::{watch, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Downloading,
    Done,
    /// Terminal. The error is carried alongside, because "it stopped" without a reason is not
    /// something a user can act on.
    Failed,
}

impl DownloadStatus {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, DownloadStatus::Downloading)
    }
}

/// Which catalogue a download belongs to.
///
/// One manager serves both, so that a double-click cannot start two transfers of anything. But
/// progress is streamed by *per-catalogue* routes, so a client reading `GET /v1/downloads` has to
/// know which route can watch a given entry. Without this, a speaker-model download listed there
/// was watched through `/v1/models/:name/download` and answered `400 not in the registry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadKind {
    /// A Whisper model, watchable at `/v1/models/:name/download`.
    Transcription,
    /// A speaker-embedding model, belonging to `/v1/speaker-models`.
    Speaker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DownloadState {
    pub model: String,
    pub kind: DownloadKind,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    /// Precomputed so every client does not reimplement the same division — including the
    /// zero-total case, which is where that division goes wrong.
    pub percent: u8,
    pub status: DownloadStatus,
    pub error: Option<String>,
}

impl DownloadState {
    fn starting(artifact: &Artifact, kind: DownloadKind) -> Self {
        Self {
            model: artifact.name.clone(),
            kind,
            downloaded_bytes: 0,
            total_bytes: artifact.bytes,
            percent: 0,
            status: DownloadStatus::Downloading,
            error: None,
        }
    }

    /// A model that is already on disk.
    pub(crate) fn already_installed(model: &ModelInfo) -> Self {
        Self::done(&model.name, model.bytes, DownloadKind::Transcription)
    }

    /// Any artifact that is already on disk.
    pub(crate) fn artifact_installed(artifact: &Artifact, kind: DownloadKind) -> Self {
        Self::done(&artifact.name, artifact.bytes, kind)
    }

    fn done(model: &str, total: u64, kind: DownloadKind) -> Self {
        Self {
            model: model.to_string(),
            kind,
            downloaded_bytes: total,
            total_bytes: total,
            percent: 100,
            status: DownloadStatus::Done,
            error: None,
        }
    }

    fn failed(model: &str, total: u64, downloaded: u64, error: String, kind: DownloadKind) -> Self {
        Self {
            model: model.to_string(),
            kind,
            downloaded_bytes: downloaded,
            total_bytes: total,
            percent: percent(downloaded, total),
            status: DownloadStatus::Failed,
            error: Some(error),
        }
    }
}

fn percent(downloaded: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    ((downloaded.min(total) as f64 / total as f64) * 100.0).round() as u8
}

/// Tracks every download this engine has started.
///
/// Finished entries are kept rather than removed. A client that subscribes a moment after a
/// small model finishes would otherwise get nothing and wait forever for an event that already
/// happened.
#[derive(Debug, Default)]
pub struct DownloadManager {
    /// `watch::Sender` is not `Clone`, so the map and the download task share one through an
    /// `Arc`. Subscribers come off the same sender, which is what lets a client attach after
    /// a download has already started — or already finished.
    active: Mutex<HashMap<String, Arc<watch::Sender<DownloadState>>>>,
}

impl DownloadManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a download, or return the one already running.
    ///
    /// Idempotent on purpose: a double-clicked button, a retried request, and a second window
    /// must not each start their own transfer of the same 3 GB file.
    pub async fn start(&self, model: ModelInfo, store: ModelStore) -> DownloadState {
        self.start_artifact(model.artifact(), store, DownloadKind::Transcription)
            .await
    }

    /// Start a download of any artifact, or return the one already running.
    ///
    /// Keyed by kind *and* name. Keying on name alone would let a speaker model and a Whisper
    /// model of the same name share one entry — and since the two are watched through different
    /// routes, the wrong one would answer.
    pub async fn start_artifact(
        &self,
        artifact: Artifact,
        store: ModelStore,
        kind: DownloadKind,
    ) -> DownloadState {
        let mut active = self.active.lock().await;
        let key = Self::key(kind, &artifact.name);

        if let Some(sender) = active.get(&key) {
            let current = sender.borrow().clone();
            // A finished or failed entry is not a running download; let it be retried.
            if !current.status.is_terminal() {
                return current;
            }
        }

        let initial = DownloadState::starting(&artifact, kind);
        let (sender, _) = watch::channel(initial.clone());
        let sender = Arc::new(sender);
        active.insert(key, Arc::clone(&sender));

        let task_sender = sender;
        let name = artifact.name.clone();
        let total = artifact.bytes;

        tokio::spawn(async move {
            let progress_sender = Arc::clone(&task_sender);
            let progress_name = name.clone();

            let result = store
                .fetch(&artifact, move |p| {
                    // `send_replace`, not `send`: `watch::Sender::send` fails when no receiver
                    // exists, leaving the stored value at whatever was last delivered. Progress
                    // is read two ways — streamed to a subscriber *and* read back through
                    // `borrow()` by `state`/`all` — so a state nobody is streaming must still be
                    // recorded. With `send` a download that finished before anyone subscribed
                    // stayed "downloading, 0%" forever, and the idempotency guard then refused to
                    // restart it because that is not a terminal state.
                    progress_sender.send_replace(DownloadState {
                        model: progress_name.clone(),
                        kind,
                        downloaded_bytes: p.downloaded_bytes,
                        total_bytes: p.total_bytes,
                        percent: percent(p.downloaded_bytes, p.total_bytes),
                        status: DownloadStatus::Downloading,
                        error: None,
                    });
                })
                .await;

            let final_state = match result {
                Ok(_) => {
                    tracing::info!(model = %name, "download finished");
                    DownloadState::done(&name, total, kind)
                }
                Err(e) => {
                    tracing::error!(model = %name, error = %e, "download failed");
                    let downloaded = task_sender.borrow().downloaded_bytes;
                    DownloadState::failed(&name, total, downloaded, e.to_string(), kind)
                }
            };
            task_sender.send_replace(final_state);
        });

        initial
    }

    /// How an entry is keyed, so the two catalogues cannot shadow each other.
    fn key(kind: DownloadKind, name: &str) -> String {
        match kind {
            DownloadKind::Transcription => name.to_string(),
            DownloadKind::Speaker => format!("speaker:{name}"),
        }
    }

    /// Subscribe to a Whisper model's progress, if it exists.
    pub async fn subscribe(&self, model: &str) -> Option<watch::Receiver<DownloadState>> {
        self.active
            .lock()
            .await
            .get(&Self::key(DownloadKind::Transcription, model))
            .map(|s| s.subscribe())
    }

    /// The current state of one Whisper model download.
    pub async fn state(&self, model: &str) -> Option<DownloadState> {
        self.active
            .lock()
            .await
            .get(&Self::key(DownloadKind::Transcription, model))
            .map(|s| s.borrow().clone())
    }

    /// Every download this engine knows about.
    pub async fn all(&self) -> Vec<DownloadState> {
        let mut states: Vec<DownloadState> = self
            .active
            .lock()
            .await
            .values()
            .map(|s| s.borrow().clone())
            .collect();
        states.sort_by(|a, b| a.model.cmp(&b.model));
        states
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_is_bounded_and_safe_at_zero() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(10, 0), 0);
        assert_eq!(percent(50, 100), 50);
        assert_eq!(percent(100, 100), 100);
        // Content-Length can under-report; a bar must not exceed full.
        assert_eq!(percent(150, 100), 100);
    }

    #[test]
    fn only_downloading_is_non_terminal() {
        assert!(!DownloadStatus::Downloading.is_terminal());
        assert!(DownloadStatus::Done.is_terminal());
        assert!(DownloadStatus::Failed.is_terminal());
    }

    /// A failure must carry its reason. "It stopped" is not something a user can act on.
    #[test]
    fn a_failed_state_keeps_the_error_and_the_progress_it_reached() {
        let state = DownloadState::failed(
            "base.en",
            1000,
            400,
            "connection reset".into(),
            DownloadKind::Transcription,
        );
        assert_eq!(state.status, DownloadStatus::Failed);
        assert_eq!(state.error.as_deref(), Some("connection reset"));
        assert_eq!(state.downloaded_bytes, 400);
        assert_eq!(state.percent, 40);
    }

    #[tokio::test]
    async fn an_unknown_download_has_no_state_and_no_subscription() {
        let manager = DownloadManager::new();
        assert!(manager.state("nope").await.is_none());
        assert!(manager.subscribe("nope").await.is_none());
        assert!(manager.all().await.is_empty());
    }

    /// An artifact that cannot be fetched, for tests that must not touch the network.
    fn unreachable(name: &str) -> Artifact {
        Artifact {
            name: name.into(),
            filename: format!("{name}.bin"),
            url: "https://notewise.invalid/nope.bin".into(),
            bytes: 128,
        }
    }

    /// Every tracked download must say which catalogue it belongs to.
    ///
    /// Regression: the shared registry listed a speaker-model download in `GET /v1/downloads`, the
    /// desktop app watched whatever was running through `/v1/models/:name/download`, and the
    /// engine answered `400 not in the registry` twice on every settings page load.
    #[tokio::test]
    async fn a_tracked_download_carries_its_catalogue() {
        let manager = DownloadManager::new();
        let dir = std::env::temp_dir().join(format!("notewise-kind-{}", std::process::id()));

        manager
            .start_artifact(
                unreachable("campplus-voxceleb"),
                ModelStore::new(&dir),
                DownloadKind::Speaker,
            )
            .await;

        let all = manager.all().await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, DownloadKind::Speaker);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The two catalogues must not share an entry, or the wrong one answers.
    ///
    /// `state`/`subscribe` are the transcription-side lookups, so a speaker download of the same
    /// name must be invisible to them — otherwise a client watching a Whisper model would be
    /// handed a speaker model's progress.
    #[tokio::test]
    async fn a_speaker_download_does_not_shadow_a_whisper_one() {
        let manager = DownloadManager::new();
        let dir = std::env::temp_dir().join(format!("notewise-shadow-{}", std::process::id()));

        // Deliberately the same name in both catalogues — the collision this keys against.
        manager
            .start_artifact(
                unreachable("collide"),
                ModelStore::new(&dir),
                DownloadKind::Speaker,
            )
            .await;

        assert!(
            manager.state("collide").await.is_none(),
            "the transcription lookup must not see a speaker download"
        );
        assert!(manager.subscribe("collide").await.is_none());

        manager
            .start_artifact(
                unreachable("collide"),
                ModelStore::new(&dir),
                DownloadKind::Transcription,
            )
            .await;

        assert_eq!(
            manager.state("collide").await.map(|s| s.kind),
            Some(DownloadKind::Transcription),
            "now the transcription entry exists and is the one returned"
        );
        assert_eq!(manager.all().await.len(), 2, "both are tracked separately");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A download nobody is watching must still record how it ended.
    ///
    /// Regression, and the subtle one: `watch::Sender::send` returns `Err` when every receiver has
    /// been dropped, so with no subscriber the stored value never advanced past `starting`. The
    /// entry sat at "downloading, 0%" forever — and because that is not terminal, the idempotency
    /// guard then refused to retry it, so the model could never be downloaded again either. It was
    /// invisible for Whisper models only because that UI always opens the progress stream.
    #[tokio::test]
    async fn a_download_with_no_subscriber_still_reaches_a_terminal_state() {
        let manager = DownloadManager::new();
        let dir = std::env::temp_dir().join(format!("notewise-dl-{}", std::process::id()));

        // A host that cannot resolve, so this fails quickly and offline.
        manager
            .start_artifact(
                unreachable("unreachable"),
                ModelStore::new(&dir),
                DownloadKind::Transcription,
            )
            .await;

        // Deliberately no `subscribe()` — that is the condition being tested.
        let mut waited = 0;
        loop {
            let state = manager.state("unreachable").await.expect("an entry");
            if state.status.is_terminal() {
                assert_eq!(state.status, DownloadStatus::Failed);
                assert!(state.error.is_some(), "a failure must carry its reason");
                break;
            }
            assert!(waited < 100, "state never became terminal: {state:?}");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            waited += 1;
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
