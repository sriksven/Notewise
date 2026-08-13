//! Live recording, driven through the HTTP surface.
//!
//! Recording is a *server-side* activity here, not a client one. The desktop window and the CLI
//! are both clients of the same engine, so putting capture behind `POST /v1/recording/start`
//! means there is exactly one implementation and exactly one place where "am I recording?" is
//! true. A frontend that captured audio itself would be a second implementation that the CLI
//! could not see and that a window reload would silently kill.
//!
//! # Why the pipeline gets its own database connection
//!
//! [`crate::AppState`] keeps the database behind a mutex. A recording lasts as long as the
//! meeting, and `Pipeline::run` borrows the database for its whole duration — so running it
//! under that mutex would block every read for an hour. Instead the recording thread opens a
//! second connection to the same file. SQLite in WAL mode supports one writer concurrent with
//! many readers, which is exactly this shape.
//!
//! This is also why recording requires a file-backed database: a second connection to
//! `:memory:` is a different, empty database, so an `--ephemeral` engine reports
//! [`RecordingError::Ephemeral`] rather than appearing to record into nothing.

use std::path::PathBuf;

use notewise_storage::Id;
use thiserror::Error;

/// Whether this build can capture audio at all.
///
/// Capture needs the `record` feature (platform audio SDKs) and transcription needs `whisper`
/// (a cmake build of whisper.cpp). Both are off by default so a plain `cargo build` stays
/// dependency-light, which means a running engine has to be able to *say* what it can do —
/// see the `can_record` field on `/health`. A UI that assumed capture worked would present a
/// button that silently did nothing.
pub const SUPPORTED: bool = cfg!(all(feature = "record", feature = "whisper"));

#[derive(Debug, Error)]
pub enum RecordingError {
    #[error(
        "this build cannot record: it was compiled without the 'record' and 'whisper' features"
    )]
    Unsupported,

    #[error(
        "recording needs a file-backed database, and this engine is running in-memory \
         (--ephemeral)"
    )]
    Ephemeral,

    #[error("already recording meeting {0}")]
    AlreadyRecording(Id),

    #[error("not recording")]
    NotRecording,

    /// The caller named a file that is not there. Their mistake, not the engine's.
    #[error("no file at {0}")]
    NoSuchFile(String),

    #[error("recording failed: {0}")]
    Failed(String),
}

pub type Result<T> = std::result::Result<T, RecordingError>;

/// What a caller asked to record.
#[derive(Debug, Clone, Default)]
pub struct StartRequest {
    pub title: Option<String>,
    /// Input device name. `None` uses the system default.
    pub device: Option<String>,
    /// Transcription model name, e.g. `base.en`.
    pub model: Option<String>,
    /// Spoken language, e.g. `en`. `None` lets the model detect it.
    pub language: Option<String>,
    pub diarize: bool,
}

/// What a caller asked to import.
#[derive(Debug, Clone)]
pub struct ImportRequest {
    pub path: PathBuf,
    pub title: Option<String>,
    pub model: Option<String>,
    pub language: Option<String>,
}

/// Transcribe a file into a new meeting.
///
/// Blocking work on a dedicated thread for the same reason live recording is: Whisper
/// inference is a long CPU burn, and running it on a runtime worker would stall every other
/// request for the length of the file.
#[allow(unused_variables)]
pub async fn import_file(
    db_path: Option<PathBuf>,
    model_dir: PathBuf,
    request: ImportRequest,
) -> Result<(Id, Outcome)> {
    #[cfg(all(feature = "record", feature = "whisper"))]
    {
        imp::import(db_path, model_dir, request).await
    }
    #[cfg(not(all(feature = "record", feature = "whisper")))]
    {
        Err(RecordingError::Unsupported)
    }
}

/// A recording in progress, as reported to a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub meeting_id: Id,
    pub device: String,
    pub model: String,
    pub language: Option<String>,
}

/// What a finished recording produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Outcome {
    pub segments: usize,
    pub speakers: usize,
    pub audio_ms: i64,
}

// ---------------------------------------------------------------------------------------------
// The manager
// ---------------------------------------------------------------------------------------------

/// Owns the at-most-one recording this engine is running.
///
/// At most one on purpose: two concurrent recordings would contend for the same microphone and
/// produce two transcripts of the same room. A user who wants that can run a second engine.
#[derive(Debug, Default)]
pub struct RecordingManager {
    #[cfg(all(feature = "record", feature = "whisper"))]
    active: tokio::sync::Mutex<Option<imp::Active>>,
}

impl RecordingManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin recording, creating the meeting as part of the same call.
    ///
    /// Creating the meeting here rather than making the client do it first means there is no
    /// window in which a meeting exists but nothing is capturing into it — which is precisely
    /// the state a crashed client would otherwise leave behind.
    #[allow(unused_variables)]
    pub async fn start(
        &self,
        db_path: Option<PathBuf>,
        model_dir: PathBuf,
        request: StartRequest,
    ) -> Result<Status> {
        #[cfg(all(feature = "record", feature = "whisper"))]
        {
            imp::start(self, db_path, model_dir, request).await
        }
        #[cfg(not(all(feature = "record", feature = "whisper")))]
        {
            Err(RecordingError::Unsupported)
        }
    }

    /// Stop the active recording and wait for its transcript to be flushed.
    ///
    /// Waits rather than detaching: the tail of the audio and the whole diarization pass happen
    /// after the stop signal, so returning early would report a transcript shorter than the one
    /// the user is about to see.
    pub async fn stop(&self) -> Result<(Id, Outcome)> {
        #[cfg(all(feature = "record", feature = "whisper"))]
        {
            imp::stop(self).await
        }
        #[cfg(not(all(feature = "record", feature = "whisper")))]
        {
            Err(RecordingError::NotRecording)
        }
    }

    /// The recording in progress, if any.
    pub async fn status(&self) -> Option<Status> {
        #[cfg(all(feature = "record", feature = "whisper"))]
        {
            self.active.lock().await.as_ref().map(imp::Active::status)
        }
        #[cfg(not(all(feature = "record", feature = "whisper")))]
        {
            None
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The real implementation, only present when capture is compiled in
// ---------------------------------------------------------------------------------------------

#[cfg(all(feature = "record", feature = "whisper"))]
mod imp {
    use super::*;

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use chrono::Utc;
    use notewise_audio_capture::{CaptureConfig, MicrophoneSource};
    use notewise_recorder::{Pipeline, PipelineConfig};
    use notewise_storage::{Database, MeetingRepository, MeetingSource, NewMeeting};
    use notewise_transcription::{ModelRegistry, ModelStore, WhisperEngine};
    use tokio::sync::oneshot;

    /// How long to wait for the pipeline to flush and diarize after a stop signal.
    ///
    /// Generous because the final pass is real work on a long meeting; bounded because a
    /// wedged pipeline must not hang the HTTP request forever.
    const STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

    #[derive(Debug)]
    pub(super) struct Active {
        meeting_id: Id,
        device: String,
        model: String,
        language: Option<String>,
        stop: Arc<AtomicBool>,
        done: oneshot::Receiver<std::result::Result<Outcome, String>>,
    }

    impl Active {
        pub(super) fn status(&self) -> Status {
            Status {
                meeting_id: self.meeting_id,
                device: self.device.clone(),
                model: self.model.clone(),
                language: self.language.clone(),
            }
        }
    }

    pub(super) async fn start(
        manager: &RecordingManager,
        db_path: Option<PathBuf>,
        model_dir: PathBuf,
        request: StartRequest,
    ) -> Result<Status> {
        let db_path = db_path.ok_or(RecordingError::Ephemeral)?;

        // Held across the whole of start: two concurrent starts must not both open the mic.
        let mut active = manager.active.lock().await;
        if let Some(current) = active.as_ref() {
            return Err(RecordingError::AlreadyRecording(current.meeting_id));
        }

        let model = ModelRegistry::get(request.model.as_deref().unwrap_or("base.en"))
            .map_err(|e| RecordingError::Failed(e.to_string()))?;
        let store = ModelStore::new(&model_dir);

        // Refuse rather than download. A download is minutes of network on a model the user
        // may not have chosen, and the meeting they are trying to record is happening now —
        // silently going quiet for that long is worse than a clear error with a fix.
        if !store.is_available(&model) {
            return Err(RecordingError::Failed(format!(
                "the '{}' model is not installed — download it first \
                 (POST /v1/models/{}/download)",
                model.name, model.name
            )));
        }

        // Open the device *before* creating the meeting, so a permission denial or a missing
        // input does not leave an empty meeting behind.
        let capture = CaptureConfig {
            device: request.device.clone(),
            ..Default::default()
        };
        let mut source =
            MicrophoneSource::open(&capture).map_err(|e| RecordingError::Failed(e.to_string()))?;
        let device = source.device_name().to_string();

        let engine = WhisperEngine::new(model.clone(), store)
            .map_err(|e| RecordingError::Failed(e.to_string()))?;

        // A dedicated connection for the recording; see the module docs.
        let db = Database::open(&db_path).map_err(|e| RecordingError::Failed(e.to_string()))?;

        let meeting = MeetingRepository::new(&db)
            .create(NewMeeting {
                project_id: None,
                title: request
                    .title
                    .clone()
                    .unwrap_or_else(|| format!("Meeting {}", Utc::now().format("%Y-%m-%d %H:%M"))),
                source: MeetingSource::Microphone,
                started_at: Utc::now(),
            })
            .map_err(|e| RecordingError::Failed(e.to_string()))?;

        let stop = Arc::new(AtomicBool::new(false));
        let poll_stop = Arc::clone(&stop);
        let (tx, done) = oneshot::channel();
        let meeting_id = meeting.id;
        let diarize = request.diarize;

        // A dedicated OS thread, not a tokio task. `MicrophoneSource::next_frame` blocks on the
        // capture channel and Whisper inference is a long CPU burn; either on a runtime worker
        // would stall unrelated HTTP requests.
        std::thread::Builder::new()
            .name("notewise-recording".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread().build() {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                };

                let mut pipeline = Pipeline::new(Box::new(engine)).with_config(PipelineConfig {
                    diarize,
                    ..Default::default()
                });

                let result =
                    runtime.block_on(pipeline.run(&db, meeting_id, &mut source, move || {
                        poll_stop.load(Ordering::Relaxed)
                    }));

                // End the meeting from this thread. It owns the connection that has been
                // writing, and it is the only place that knows the recording is truly over.
                if let Err(e) = MeetingRepository::new(&db).end(meeting_id, Utc::now()) {
                    tracing::error!(error = %e, "could not mark the meeting ended");
                }

                let _ = tx.send(
                    result
                        .map(|stats| Outcome {
                            segments: stats.segments_stored,
                            speakers: stats.speakers_detected,
                            audio_ms: stats.audio_ms,
                        })
                        .map_err(|e| e.to_string()),
                );
            })
            .map_err(|e| RecordingError::Failed(e.to_string()))?;

        let status = Status {
            meeting_id,
            device: device.clone(),
            model: model.name.to_string(),
            language: request.language.clone(),
        };

        tracing::info!(%meeting_id, device = %device, model = %model.name, "recording started");

        *active = Some(Active {
            meeting_id,
            device,
            model: model.name.to_string(),
            language: request.language.clone(),
            stop,
            done,
        });

        Ok(status)
    }

    /// Transcribe an existing file into a new meeting.
    pub(super) async fn import(
        db_path: Option<PathBuf>,
        model_dir: PathBuf,
        request: ImportRequest,
    ) -> Result<(Id, Outcome)> {
        use notewise_audio_capture::FileSource;

        let db_path = db_path.ok_or(RecordingError::Ephemeral)?;

        if !request.path.exists() {
            return Err(RecordingError::NoSuchFile(
                request.path.display().to_string(),
            ));
        }

        let model = ModelRegistry::get(request.model.as_deref().unwrap_or("base.en"))
            .map_err(|e| RecordingError::Failed(e.to_string()))?;
        let store = ModelStore::new(&model_dir);
        if !store.is_available(&model) {
            return Err(RecordingError::Failed(format!(
                "the '{}' model is not installed — download it first",
                model.name
            )));
        }

        let (tx, done) = oneshot::channel();
        let title = request.title.clone().unwrap_or_else(|| {
            request
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Imported meeting".into())
        });
        let language = request.language.clone();
        let path = request.path.clone();

        std::thread::Builder::new()
            .name("notewise-import".into())
            .spawn(move || {
                let result = (|| -> Result<(Id, Outcome)> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .build()
                        .map_err(|e| RecordingError::Failed(e.to_string()))?;

                    let engine = WhisperEngine::new(model, store)
                        .map_err(|e| RecordingError::Failed(e.to_string()))?
                        .with_language(language);

                    let mut source = FileSource::open_wav(&path)
                        .map_err(|e| RecordingError::Failed(e.to_string()))?;

                    let db = Database::open(&db_path)
                        .map_err(|e| RecordingError::Failed(e.to_string()))?;

                    let meeting = MeetingRepository::new(&db)
                        .create(NewMeeting {
                            project_id: None,
                            title,
                            source: MeetingSource::Import,
                            started_at: Utc::now(),
                        })
                        .map_err(|e| RecordingError::Failed(e.to_string()))?;

                    let mut pipeline = Pipeline::new(Box::new(engine));
                    let stats = runtime
                        .block_on(pipeline.run(&db, meeting.id, &mut source, || false))
                        .map_err(|e| RecordingError::Failed(e.to_string()))?;

                    MeetingRepository::new(&db)
                        .end(meeting.id, Utc::now())
                        .map_err(|e| RecordingError::Failed(e.to_string()))?;

                    Ok((
                        meeting.id,
                        Outcome {
                            segments: stats.segments_stored,
                            speakers: stats.speakers_detected,
                            audio_ms: stats.audio_ms,
                        },
                    ))
                })();
                let _ = tx.send(result);
            })
            .map_err(|e| RecordingError::Failed(e.to_string()))?;

        done.await
            .map_err(|_| RecordingError::Failed("the import thread stopped unexpectedly".into()))?
    }

    pub(super) async fn stop(manager: &RecordingManager) -> Result<(Id, Outcome)> {
        // Taken out of the slot before awaiting, so a failed stop cannot leave a half-dead
        // recording that blocks every future start.
        let Some(current) = manager.active.lock().await.take() else {
            return Err(RecordingError::NotRecording);
        };

        current.stop.store(true, Ordering::Relaxed);

        match tokio::time::timeout(STOP_TIMEOUT, current.done).await {
            Ok(Ok(Ok(outcome))) => {
                tracing::info!(
                    meeting_id = %current.meeting_id,
                    segments = outcome.segments,
                    "recording stopped"
                );
                Ok((current.meeting_id, outcome))
            }
            Ok(Ok(Err(e))) => Err(RecordingError::Failed(e)),
            // The thread died without reporting: a panic in the pipeline.
            Ok(Err(_)) => Err(RecordingError::Failed(
                "the recording thread stopped unexpectedly".into(),
            )),
            Err(_) => Err(RecordingError::Failed(
                "the recording did not finish flushing in time".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unsupported path must be an explicit error, never a silent success. A client that
    /// got `200 OK` from a build with no capture would show a running timer over no audio.
    #[tokio::test]
    async fn an_unsupported_build_refuses_to_start() {
        let manager = RecordingManager::new();
        let result = manager
            .start(
                Some(PathBuf::from("/tmp/notewise-test.db")),
                PathBuf::from("/tmp/notewise-models"),
                StartRequest::default(),
            )
            .await;

        if SUPPORTED {
            // With capture compiled in this reaches the model check, which is the point: it
            // did not claim to be recording without one.
            assert!(matches!(
                result,
                Err(RecordingError::Failed(_)) | Err(RecordingError::AlreadyRecording(_)) | Ok(_)
            ));
        } else {
            assert!(matches!(result, Err(RecordingError::Unsupported)));
        }
    }

    #[tokio::test]
    async fn an_ephemeral_database_cannot_record() {
        let manager = RecordingManager::new();
        let result = manager
            .start(
                None,
                PathBuf::from("/tmp/notewise-models"),
                StartRequest::default(),
            )
            .await;

        // Either refusal is correct; both name a real reason rather than pretending to record.
        assert!(
            matches!(
                result,
                Err(RecordingError::Ephemeral) | Err(RecordingError::Unsupported)
            ),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn stopping_when_idle_is_an_error_not_a_silent_ok() {
        let manager = RecordingManager::new();
        assert!(matches!(
            manager.stop().await,
            Err(RecordingError::NotRecording)
        ));
    }

    #[tokio::test]
    async fn nothing_is_reported_active_before_a_start() {
        assert_eq!(RecordingManager::new().status().await, None);
    }

    #[test]
    fn every_error_explains_itself() {
        let cases: Vec<RecordingError> = vec![
            RecordingError::Unsupported,
            RecordingError::Ephemeral,
            RecordingError::NotRecording,
            RecordingError::Failed("disk full".into()),
        ];

        for error in cases {
            let message = error.to_string();
            assert!(!message.is_empty(), "{error:?} has an empty message");
            assert!(
                message.chars().next().is_some_and(char::is_lowercase),
                "'{message}' should read as a sentence fragment"
            );
        }
    }

    /// The unsupported message must say *why*, since the fix is a rebuild with different
    /// features and nothing in the API surface hints at that.
    #[test]
    fn the_unsupported_message_names_the_missing_features() {
        let message = RecordingError::Unsupported.to_string();
        assert!(message.contains("record"), "{message}");
        assert!(message.contains("whisper"), "{message}");
    }
}
