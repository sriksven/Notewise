//! Turning acoustic speaker separation on, and the model it needs.
//!
//! # Why this is off by default
//!
//! Clustering voice embeddings is the only way to answer "who spoke" for a mono recording with no
//! platform timeline — an imported file, or one microphone in a room. It is also a guess, and its
//! accuracy on real meetings is not something this repository has measured: see the note on
//! [`notewise_diarization::models`]. A guess presented as a finding is worse than the honest
//! `Speaker 1`, so a user turns this on deliberately, having been told what it is.
//!
//! # Three things have to be true before it runs
//!
//! The build must have the `speaker-diarization` feature, the model must be downloaded, and the
//! setting must be on. Each is reported separately by [`status`], because "nothing happened" with
//! one explanation covering three causes is not something anyone can act on.
//!
//! # Where it applies
//!
//! Imports. Live channel recording already knows who spoke from *which stream* the audio arrived
//! on, which is exact and free; overwriting that with a clustering guess would be a downgrade.
//! Splitting several people *within* one channel is a different job, and the browser extension's
//! timeline is the better evidence for it when it is available.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use notewise_diarization::{
    AudioDiarizer, EmbeddingDiarizer, SpeakerEmbedder, SpeakerModelRegistry,
};
use notewise_storage::SettingsRepository;

use crate::downloads::DownloadState;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

type Shared = std::sync::Arc<AppState>;

/// Setting key: whether acoustic separation runs. `"off"` or `"acoustic"`.
pub const MODE_KEY: &str = "diarization.mode";
/// Setting key: which speaker model to use.
pub const MODEL_KEY: &str = "diarization.model";
/// Setting key: how much audio may be held in memory for the acoustic pass, in minutes.
pub const RETAIN_MINUTES_KEY: &str = "diarization.retain_minutes";

/// Default memory budget for retained audio.
///
/// Mono 16 kHz `f32` is 64 KB per second, so 90 minutes is roughly 345 MB. Chosen to cover a
/// long meeting rather than to be generous: past the budget the acoustic pass is skipped
/// entirely, which is a better failure than labelling the tail from audio nobody examined.
pub const DEFAULT_RETAIN_MINUTES: i64 = 90;

/// Largest budget a caller may set, to keep a typo from trying to hold 40 GB.
const MAX_RETAIN_MINUTES: i64 = 8 * 60;

/// How speakers get separated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// One speaker, no separation attempted. What the transcript says is not in doubt.
    Off,
    /// Cluster voice embeddings from the retained audio.
    Acoustic,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Acoustic => "acoustic",
        }
    }

    /// Parse a stored value, treating anything unrecognised as off.
    ///
    /// Deliberately lenient in this direction only: an unreadable setting must not silently
    /// enable a guess, and it must not fail the request either — a corrupt row would otherwise
    /// make every import fail rather than just decline to separate speakers.
    fn parse(raw: &str) -> Self {
        match raw {
            "acoustic" => Mode::Acoustic,
            _ => Mode::Off,
        }
    }
}

/// What this engine can and will do about speakers.
#[derive(Debug, Serialize)]
pub struct Status {
    pub mode: Mode,
    pub model: String,
    pub retain_minutes: i64,
    /// Whether this build has the feature compiled in.
    pub supported: bool,
    /// Whether the chosen model is on disk.
    pub model_installed: bool,
    /// Whether all three conditions hold, so separation will actually happen on the next import.
    pub effective: bool,
    /// Why it will not run, when it will not. `null` when it will.
    pub blocked_by: Option<&'static str>,
}

/// Read the current settings, defaulting where unset.
pub fn settings(db: &notewise_storage::Database) -> Result<(Mode, String, i64), ApiError> {
    let repo = SettingsRepository::new(db);

    let mode = repo
        .get(MODE_KEY)?
        .as_deref()
        .map(Mode::parse)
        .unwrap_or(Mode::Off);

    let model = repo
        .get(MODEL_KEY)?
        .unwrap_or_else(|| SpeakerModelRegistry::default_model().name.to_string());

    let retain = repo
        .get(RETAIN_MINUTES_KEY)?
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|m| *m > 0 && *m <= MAX_RETAIN_MINUTES)
        .unwrap_or(DEFAULT_RETAIN_MINUTES);

    Ok((mode, model, retain))
}

/// `GET /v1/diarization`
pub async fn get_status(State(state): State<Shared>) -> ApiResult<Json<Status>> {
    let store = state.model_store();
    let db = state.db().await;
    let (mode, model, retain_minutes) = settings(&db)?;
    drop(db);

    // An unknown model name is reported as not installed rather than as an error: the setting is
    // stale, and the fix is choosing a model, not a 500.
    let model_installed = SpeakerModelRegistry::get(&model)
        .map(|m| store.has_artifact(&m.artifact()))
        .unwrap_or(false);

    let supported = notewise_diarization::acoustic_available();
    let blocked_by = if mode == Mode::Off {
        Some("Speaker separation is turned off.")
    } else if !supported {
        Some("This build was compiled without acoustic speaker separation.")
    } else if !model_installed {
        Some("The speaker model has not been downloaded yet.")
    } else {
        None
    };

    Ok(Json(Status {
        mode,
        model,
        retain_minutes,
        supported,
        model_installed,
        effective: blocked_by.is_none(),
        blocked_by,
    }))
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettings {
    #[serde(default)]
    pub mode: Option<Mode>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub retain_minutes: Option<i64>,
}

/// `PUT /v1/diarization`
///
/// Turning it on in a build that cannot do it, or with a model that is not downloaded, is
/// allowed: the setting is a statement of intent, and [`Status::blocked_by`] already explains why
/// nothing is happening yet. Rejecting it would mean a user cannot express the preference before
/// the download finishes.
pub async fn update_status(
    State(state): State<Shared>,
    Json(body): Json<UpdateSettings>,
) -> ApiResult<Json<Status>> {
    // Validate before writing anything, so a bad field cannot leave half the change applied.
    if let Some(name) = &body.model {
        SpeakerModelRegistry::get(name).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    }
    if let Some(minutes) = body.retain_minutes {
        if minutes <= 0 || minutes > MAX_RETAIN_MINUTES {
            return Err(ApiError::BadRequest(format!(
                "retain_minutes must be between 1 and {MAX_RETAIN_MINUTES}"
            )));
        }
    }

    {
        let db = state.db().await;
        let repo = SettingsRepository::new(&db);
        if let Some(mode) = body.mode {
            repo.set(MODE_KEY, mode.as_str())?;
        }
        if let Some(model) = &body.model {
            repo.set(MODEL_KEY, model)?;
        }
        if let Some(minutes) = body.retain_minutes {
            repo.set(RETAIN_MINUTES_KEY, &minutes.to_string())?;
        }
    }

    get_status(State(state)).await
}

/// `GET /v1/speaker-models`
pub async fn list_models(State(state): State<Shared>) -> ApiResult<Json<serde_json::Value>> {
    let store = state.model_store();
    let db = state.db().await;
    let (_, chosen, _) = settings(&db)?;
    drop(db);

    let models: Vec<_> = SpeakerModelRegistry::all()
        .into_iter()
        .map(|model| {
            serde_json::json!({
                "name": model.name,
                "bytes": model.bytes,
                "approx_mb": model.approx_mb(),
                "installed": store.has_artifact(&model.artifact()),
                "selected": model.name == chosen,
                "recommended": model.name == SpeakerModelRegistry::default_model().name,
                "tradeoff": model.tradeoff,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "models": models,
        "directory": store.dir().display().to_string(),
        "supported": notewise_diarization::acoustic_available(),
    })))
}

/// `POST /v1/speaker-models/:name/download`
pub async fn download_model(
    State(state): State<Shared>,
    Path(name): Path<String>,
) -> ApiResult<(axum::http::StatusCode, Json<DownloadState>)> {
    let model =
        SpeakerModelRegistry::get(&name).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let store = state.model_store();
    let artifact = model.artifact();

    if store.has_artifact(&artifact) {
        return Ok((
            axum::http::StatusCode::OK,
            Json(DownloadState::artifact_installed(
                &artifact,
                crate::downloads::DownloadKind::Speaker,
            )),
        ));
    }

    let started = state
        .downloads()
        .start_artifact(artifact, store, crate::downloads::DownloadKind::Speaker)
        .await;
    Ok((axum::http::StatusCode::ACCEPTED, Json(started)))
}

/// `DELETE /v1/speaker-models/:name`
pub async fn remove_model(
    State(state): State<Shared>,
    Path(name): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let model =
        SpeakerModelRegistry::get(&name).map_err(|e| ApiError::BadRequest(e.to_string()))?;

    state
        .model_store()
        .remove_artifact(&model.artifact())
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(serde_json::json!({ "removed": model.name })))
}

/// Build the acoustic diarizer for an import, if everything needed is in place.
///
/// Returns `None` when separation is off, unsupported, or the model is missing — all three are
/// ordinary states, not errors, and an import must go ahead without speaker labels rather than
/// fail. A model that is present but refuses to load *is* worth surfacing, so that logs a warning
/// rather than passing silently.
pub fn diarizer_for_import(
    db: &notewise_storage::Database,
    store: &notewise_transcription::ModelStore,
) -> Option<(Box<dyn AudioDiarizer + Send>, i64)> {
    let (mode, model_name, retain_minutes) = settings(db).ok()?;
    if mode != Mode::Acoustic || !notewise_diarization::acoustic_available() {
        return None;
    }

    let model = SpeakerModelRegistry::get(&model_name).ok()?;
    let artifact = model.artifact();
    if !store.has_artifact(&artifact) {
        tracing::info!(
            model = model.name,
            "acoustic separation is on but the model is not downloaded; importing without it"
        );
        return None;
    }

    match SpeakerEmbedder::load(store.artifact_path(&artifact)) {
        Ok(embedder) => Some((
            Box::new(EmbeddingDiarizer::new(embedder)),
            retain_minutes * 60_000,
        )),
        Err(e) => {
            tracing::warn!(
                model = model.name,
                error = %e,
                "the speaker model is present but would not load; importing without separation"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unrecognised_mode_reads_as_off() {
        // A corrupt setting must not silently enable a guess.
        assert_eq!(Mode::parse("acoustic"), Mode::Acoustic);
        assert_eq!(Mode::parse("off"), Mode::Off);
        assert_eq!(Mode::parse("ACOUSTIC"), Mode::Off);
        assert_eq!(Mode::parse(""), Mode::Off);
        assert_eq!(Mode::parse("on"), Mode::Off);
    }

    #[test]
    fn modes_round_trip_through_their_stored_form() {
        for mode in [Mode::Off, Mode::Acoustic] {
            assert_eq!(Mode::parse(mode.as_str()), mode);
        }
    }

    #[test]
    fn defaults_apply_when_nothing_has_been_set() {
        let db = notewise_storage::Database::open_in_memory().unwrap();
        let (mode, model, retain) = settings(&db).unwrap();

        assert_eq!(mode, Mode::Off, "separation must be opt-in");
        assert_eq!(model, SpeakerModelRegistry::default_model().name);
        assert_eq!(retain, DEFAULT_RETAIN_MINUTES);
    }

    /// A nonsense budget falls back rather than being honoured — a zero would mean the acoustic
    /// pass silently never runs, and a huge one would try to hold the machine's whole memory.
    #[test]
    fn an_out_of_range_budget_falls_back_to_the_default() {
        let db = notewise_storage::Database::open_in_memory().unwrap();
        let repo = SettingsRepository::new(&db);

        for bad in ["0", "-5", "100000", "soon", ""] {
            repo.set(RETAIN_MINUTES_KEY, bad).unwrap();
            let (_, _, retain) = settings(&db).unwrap();
            assert_eq!(retain, DEFAULT_RETAIN_MINUTES, "for {bad:?}");
        }

        repo.set(RETAIN_MINUTES_KEY, "30").unwrap();
        assert_eq!(settings(&db).unwrap().2, 30);
    }

    /// Off is off regardless of what else is in place.
    #[test]
    fn no_diarizer_is_built_while_the_setting_is_off() {
        let db = notewise_storage::Database::open_in_memory().unwrap();
        let store = notewise_transcription::ModelStore::new(std::env::temp_dir());

        assert!(diarizer_for_import(&db, &store).is_none());
    }

    /// Turning it on with no model present must not build a diarizer that cannot work.
    #[test]
    fn no_diarizer_is_built_without_a_downloaded_model() {
        let db = notewise_storage::Database::open_in_memory().unwrap();
        SettingsRepository::new(&db)
            .set(MODE_KEY, "acoustic")
            .unwrap();

        let empty = std::env::temp_dir().join(format!("notewise-no-models-{}", std::process::id()));
        let store = notewise_transcription::ModelStore::new(&empty);

        assert!(diarizer_for_import(&db, &store).is_none());
    }

    /// A build without the feature must decline even with everything else in place, rather than
    /// constructing an embedder whose every call fails.
    #[test]
    #[cfg(not(feature = "speaker-diarization"))]
    fn a_build_without_the_feature_declines() {
        let db = notewise_storage::Database::open_in_memory().unwrap();
        SettingsRepository::new(&db)
            .set(MODE_KEY, "acoustic")
            .unwrap();

        assert!(!notewise_diarization::acoustic_available());
        assert!(diarizer_for_import(
            &db,
            &notewise_transcription::ModelStore::new(std::env::temp_dir())
        )
        .is_none());
    }
}
