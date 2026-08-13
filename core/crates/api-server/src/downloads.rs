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

use notewise_transcription::{ModelInfo, ModelStore};
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DownloadState {
    pub model: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    /// Precomputed so every client does not reimplement the same division — including the
    /// zero-total case, which is where that division goes wrong.
    pub percent: u8,
    pub status: DownloadStatus,
    pub error: Option<String>,
}

impl DownloadState {
    fn starting(model: &ModelInfo) -> Self {
        Self {
            model: model.name.to_string(),
            downloaded_bytes: 0,
            total_bytes: model.bytes,
            percent: 0,
            status: DownloadStatus::Downloading,
            error: None,
        }
    }

    /// A model that is already on disk.
    pub(crate) fn already_installed(model: &ModelInfo) -> Self {
        Self::done(&model.name, model.bytes)
    }

    fn done(model: &str, total: u64) -> Self {
        Self {
            model: model.to_string(),
            downloaded_bytes: total,
            total_bytes: total,
            percent: 100,
            status: DownloadStatus::Done,
            error: None,
        }
    }

    fn failed(model: &str, total: u64, downloaded: u64, error: String) -> Self {
        Self {
            model: model.to_string(),
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
        let mut active = self.active.lock().await;

        if let Some(sender) = active.get(&model.name.to_string()) {
            let current = sender.borrow().clone();
            // A finished or failed entry is not a running download; let it be retried.
            if !current.status.is_terminal() {
                return current;
            }
        }

        let initial = DownloadState::starting(&model);
        let (sender, _) = watch::channel(initial.clone());
        let sender = Arc::new(sender);
        active.insert(model.name.to_string(), Arc::clone(&sender));

        let task_sender = sender;
        let name = model.name.to_string();
        let total = model.bytes;

        tokio::spawn(async move {
            let progress_sender = Arc::clone(&task_sender);
            let progress_name = name.clone();

            let result = store
                .download_with_progress(&model, move |p| {
                    let _ = progress_sender.send(DownloadState {
                        model: progress_name.clone(),
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
                    DownloadState::done(&name, total)
                }
                Err(e) => {
                    tracing::error!(model = %name, error = %e, "download failed");
                    let downloaded = task_sender.borrow().downloaded_bytes;
                    DownloadState::failed(&name, total, downloaded, e.to_string())
                }
            };
            let _ = task_sender.send(final_state);
        });

        initial
    }

    /// Subscribe to a download's progress, if it exists.
    pub async fn subscribe(&self, model: &str) -> Option<watch::Receiver<DownloadState>> {
        self.active.lock().await.get(model).map(|s| s.subscribe())
    }

    /// The current state of one download.
    pub async fn state(&self, model: &str) -> Option<DownloadState> {
        self.active
            .lock()
            .await
            .get(model)
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
        let state = DownloadState::failed("base.en", 1000, 400, "connection reset".into());
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
}
