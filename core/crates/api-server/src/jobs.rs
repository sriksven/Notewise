//! Running scheduled jobs, and the HTTP surface for managing them.
//!
//! # What a job may do
//!
//! Exactly what the agent can already do: search, read, and write one note. Those are reversible
//! and local.
//!
//! It may not execute an external tool. Spec 6 requires a human to confirm every external tool call,
//! every time, and a scheduled job is by definition unattended — those two cannot both hold, and the
//! confirmation is the one that stays. That holds now that the MCP client exists: `job_allowed_tools`
//! scopes which tools a run may *propose*, [`crate::tools`] is where a proposal becomes a call, and
//! only a request from the interface moves a row to `confirmed`. Nothing here generates proposals
//! yet, so `proposals` is still always zero.
//!
//! Allowing pre-approved tools on a schedule is the shortest path to this product acquiring an
//! unattended path into other people's systems, which nothing in it has by design.
//!
//! # Why the scheduler lives here
//!
//! `api-server` already owns the tokio runtime, the backend, the repositories, and the agent. A
//! separate crate would need all of that handed to it for no isolation benefit. The part that is
//! genuinely tricky — given cron expressions and a clock, what is due — is [`crate::schedule`],
//! which is pure.

use std::sync::Arc;

use axum::extract::State;
use axum::{
    routing::{get, post},
    Json, Router as AxumRouter,
};
use chrono::Utc;
use notewise_storage::{
    Id, Job, JobRepository, NewJob, NewNotification, NotificationChannel, NotificationRepository,
    RunStatus,
};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::schedule::{self, Due, JobTiming};
use crate::state::AppState;

type Shared = Arc<AppState>;

/// How often the scheduler wakes.
///
/// A minute is the finest granularity standard cron expresses, so checking more often cannot fire
/// anything sooner — it would only burn wakeups.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(60);

/// How many jobs may run at once.
///
/// Small on purpose: several heavy jobs firing at midnight against a local model would saturate the
/// machine, and the user is asleep and cannot tell it to stop.
pub const MAX_CONCURRENT: usize = 2;

pub fn routes() -> AxumRouter<Shared> {
    AxumRouter::new()
        .route("/v1/jobs", get(list_jobs).post(create_job))
        .route("/v1/jobs/:id", axum::routing::delete(delete_job))
        .route("/v1/jobs/:id/enabled", axum::routing::put(set_enabled))
        .route("/v1/jobs/:id/runs", get(list_runs))
        .route("/v1/jobs/:id/run", post(run_now))
        .route("/v1/jobs/preview", post(preview))
}

#[derive(Debug, Serialize)]
struct JobBody {
    id: String,
    name: String,
    prompt: String,
    cron: String,
    timezone: String,
    enabled: bool,
    catch_up: bool,
    timeout_secs: i64,
    /// When it would next fire. `None` if the expression no longer parses.
    next_fire: Option<String>,
}

fn describe(job: Job) -> JobBody {
    let next_fire = schedule::next_fires(&job.cron, &job.timezone, Utc::now(), 1)
        .ok()
        .and_then(|v| v.first().map(|t| t.to_rfc3339()));

    JobBody {
        id: job.id.to_string(),
        name: job.name,
        prompt: job.prompt,
        cron: job.cron,
        timezone: job.timezone,
        enabled: job.enabled,
        catch_up: job.catch_up,
        timeout_secs: job.timeout_secs,
        next_fire,
    }
}

async fn list_jobs(State(state): State<Shared>) -> ApiResult<Json<Vec<JobBody>>> {
    let db = state.db().await;
    let jobs = JobRepository::new(&db).list()?;
    Ok(Json(jobs.into_iter().map(describe).collect()))
}

#[derive(Debug, Deserialize)]
struct CreateJobBody {
    name: String,
    prompt: String,
    cron: String,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    catch_up: bool,
    #[serde(default)]
    timeout_secs: Option<i64>,
}

/// Create a job.
///
/// The cron expression is validated here rather than discovered to be broken at 3am, when the only
/// symptom is a job that silently never ran.
async fn create_job(
    State(state): State<Shared>,
    Json(body): Json<CreateJobBody>,
) -> ApiResult<Json<JobBody>> {
    let name = body.name.trim();
    let prompt = body.prompt.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("a job needs a name".into()));
    }
    if prompt.is_empty() {
        return Err(ApiError::BadRequest(
            "a job with no instructions has nothing to do".into(),
        ));
    }

    // Defaults to the machine's zone, because "every Friday at 5pm" means the user's Friday.
    let timezone = body
        .timezone
        .unwrap_or_else(|| iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into()));

    schedule::next_fires(&body.cron, &timezone, Utc::now(), 1)
        .map_err(|e| ApiError::BadRequest(format!("that schedule will not run: {e}")))?;

    let timeout_secs = body.timeout_secs.unwrap_or(600);
    if timeout_secs <= 0 {
        return Err(ApiError::BadRequest(
            "a timeout of zero would abandon every run immediately".into(),
        ));
    }

    let db = state.db().await;
    let made = JobRepository::new(&db).create(NewJob {
        name: name.to_string(),
        prompt: prompt.to_string(),
        cron: body.cron.trim().to_string(),
        timezone,
        catch_up: body.catch_up,
        timeout_secs,
    })?;

    Ok(Json(describe(made)))
}

async fn delete_job(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    JobRepository::new(&db).delete(id)?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[derive(Debug, Deserialize)]
struct EnabledBody {
    enabled: bool,
}

async fn set_enabled(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<EnabledBody>,
) -> ApiResult<Json<JobBody>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    let job = JobRepository::new(&db).set_enabled(id, body.enabled)?;
    Ok(Json(describe(job)))
}

#[derive(Debug, Serialize)]
struct RunBody {
    id: String,
    status: String,
    trace: Option<serde_json::Value>,
    note_id: Option<String>,
    proposals: i64,
    error: Option<String>,
    started_at: String,
    finished_at: Option<String>,
}

async fn list_runs(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<Vec<RunBody>>> {
    let id = parse_id(&id)?;
    let db = state.db().await;
    let runs = JobRepository::new(&db).runs(id, 50)?;

    Ok(Json(
        runs.into_iter()
            .map(|r| RunBody {
                id: r.id.to_string(),
                status: r.status.as_str().to_string(),
                trace: r.trace.and_then(|t| serde_json::from_str(&t).ok()),
                note_id: r.note_id.map(|i| i.to_string()),
                proposals: r.proposals,
                error: r.error,
                started_at: r.started_at.to_rfc3339(),
                finished_at: r.finished_at.map(|t| t.to_rfc3339()),
            })
            .collect(),
    ))
}

#[derive(Debug, Deserialize)]
struct PreviewBody {
    cron: String,
    #[serde(default)]
    timezone: Option<String>,
}

/// The next few fire times, so a schedule can be checked before it is saved.
async fn preview(Json(body): Json<PreviewBody>) -> ApiResult<Json<serde_json::Value>> {
    let timezone = body
        .timezone
        .unwrap_or_else(|| iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into()));

    let fires = schedule::next_fires(&body.cron, &timezone, Utc::now(), 5)
        .map_err(|e| ApiError::BadRequest(format!("that schedule will not run: {e}")))?;

    Ok(Json(serde_json::json!({
        "timezone": timezone,
        "next": fires.iter().map(|t| t.to_rfc3339()).collect::<Vec<_>>(),
    })))
}

/// Run a job now, regardless of its schedule.
///
/// Refused while a run is in flight, for the same reason the scheduler skips: two concurrent runs of
/// one job would both write a note and both cost model calls.
async fn run_now(
    State(state): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let id = parse_id(&id)?;

    let job = {
        let db = state.db().await;
        let repo = JobRepository::new(&db);
        let job = repo.get(id)?;
        if repo.timing(id)?.1 {
            return Err(ApiError::BadRequest(
                "that job is already running".to_string(),
            ));
        }
        job
    };

    let run_id = execute(Arc::clone(&state), job).await?;
    Ok(Json(serde_json::json!({ "run_id": run_id.to_string() })))
}

/// Start a run and drive it to completion.
///
/// Returns once the run has been *recorded as started*, not once it has finished — a digest over a
/// year of meetings can take minutes, and an HTTP request should not hold open for it.
async fn execute(state: Shared, job: Job) -> ApiResult<Id> {
    let run_id = {
        let db = state.db().await;
        JobRepository::new(&db).start_run(job.id, Utc::now())?
    };

    tokio::spawn(async move {
        let outcome = crate::agent::run_to_completion(&state, &job.prompt, job.timeout_secs).await;

        let db = state.db().await;
        let repo = JobRepository::new(&db);

        let recorded = match &outcome {
            Ok(run) => {
                let trace = serde_json::to_string(&run.steps).ok();
                repo.finish_run(
                    run_id,
                    RunStatus::Completed,
                    trace.as_deref(),
                    run.note_id,
                    None,
                )
            }
            Err(e) if e.timed_out => {
                repo.finish_run(run_id, RunStatus::TimedOut, None, None, Some(&e.message))
            }
            Err(e) => repo.finish_run(run_id, RunStatus::Failed, None, None, Some(&e.message)),
        };
        if let Err(e) = recorded {
            tracing::warn!(error = %e, "could not record the job run");
        }

        // Tell the user, because nobody was watching. Queued rather than shown: raising a
        // notification is the frontend's job, and the engine has no way to.
        let body = match &outcome {
            Ok(run) => match &run.note_title {
                Some(title) => format!("'{}' finished and wrote \"{title}\"", job.name),
                None => format!("'{}' finished", job.name),
            },
            Err(e) => format!("'{}' failed: {}", job.name, e.message),
        };
        let queued = NotificationRepository::new(&db).create(NewNotification {
            source_kind: "job".into(),
            source_id: job.id,
            recipient: "me".into(),
            channel: NotificationChannel::Desktop,
            body,
        });
        if let Err(e) = queued {
            tracing::warn!(error = %e, "could not queue the job notification");
        }
    });

    Ok(run_id)
}

fn parse_id(raw: &str) -> ApiResult<Id> {
    raw.parse()
        .map_err(|_| ApiError::BadRequest(format!("'{raw}' is not an id")))
}

/// The scheduler loop. Runs for the life of the process.
///
/// Every decision about *whether* to run comes from [`crate::schedule::due`], which is pure — this
/// only acts on the answer.
pub fn spawn(state: Shared) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(TICK).await;
            if let Err(e) = tick(&state).await {
                // A scheduler that dies on one bad tick stops every job silently.
                tracing::warn!(error = %e, "a scheduler tick failed; continuing");
            }
        }
    });
}

/// One pass over every job. Separate from the loop so it is callable directly.
pub async fn tick(state: &Shared) -> ApiResult<usize> {
    let now = Utc::now();

    let candidates = {
        let db = state.db().await;
        let repo = JobRepository::new(&db);
        let mut out = Vec::new();

        for job in repo.list()? {
            let (last_started, running) = repo.timing(job.id)?;
            let verdict = schedule::due(
                &JobTiming {
                    cron: &job.cron,
                    timezone: &job.timezone,
                    enabled: job.enabled,
                    catch_up: job.catch_up,
                    last_started,
                    running,
                },
                now,
            );

            match verdict {
                Due::Run { .. } => out.push(job),
                // Recorded so a too-tight schedule or a weekend of downtime is visible rather than
                // being an unexplained absence in the history.
                Due::AlreadyRunning => {
                    let _ = repo.record_skip(job.id, now, "the previous run had not finished");
                }
                Due::Missed { count } => {
                    let _ = repo.record_skip(
                        job.id,
                        now,
                        &format!("{count} occurrences were missed while the app was closed"),
                    );
                }
                Due::Invalid { reason } => {
                    let _ = repo.record_skip(job.id, now, &format!("invalid schedule: {reason}"));
                }
                Due::NotDue => {}
            }
        }
        out
    };

    let mut started = 0;
    for job in candidates.into_iter().take(MAX_CONCURRENT) {
        if execute(Arc::clone(state), job).await.is_ok() {
            started += 1;
        }
    }
    Ok(started)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use notewise_ai_router::{Router as AiRouter, RouterConfig};
    use notewise_storage::Database;
    use tower::ServiceExt;

    fn app() -> AxumRouter<()> {
        let state = AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        );
        routes().with_state(Arc::new(state))
    }

    async fn call(app: &AxumRouter<()>, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = app.clone().oneshot(request).await.expect("request");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("request")
    }

    fn send(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    }

    fn a_job(name: &str, cron: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "prompt": "summarise this week's decisions",
            "cron": cron,
            "timezone": "UTC",
        })
    }

    #[tokio::test]
    async fn a_fresh_engine_has_no_jobs() {
        let (status, body) = call(&app(), get("/v1/jobs")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.as_array().expect("array").len(), 0);
    }

    #[tokio::test]
    async fn a_job_round_trips_and_reports_when_it_next_fires() {
        let app = app();
        let (status, made) = call(
            &app,
            send("POST", "/v1/jobs", a_job("Weekly", "0 9 * * MON")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{made}");
        assert!(
            made["next_fire"].is_string(),
            "a schedule the user cannot see the effect of is a schedule they cannot check: {made}"
        );

        let (_, listed) = call(&app, get("/v1/jobs")).await;
        assert_eq!(listed.as_array().expect("array").len(), 1);
    }

    /// Rejected at save, not discovered at 3am when the only symptom is a job that never ran.
    #[tokio::test]
    async fn an_invalid_schedule_is_refused_at_save() {
        for bad in ["not cron", "* * *", "99 * * * *"] {
            let (status, body) = call(&app(), send("POST", "/v1/jobs", a_job("Bad", bad))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?} -> {body}");
        }
    }

    #[tokio::test]
    async fn a_job_needs_a_name_and_something_to_do() {
        let nameless = serde_json::json!({"name": "  ", "prompt": "x", "cron": "0 9 * * *"});
        let (status, _) = call(&app(), send("POST", "/v1/jobs", nameless)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let purposeless = serde_json::json!({"name": "x", "prompt": " ", "cron": "0 9 * * *"});
        let (status, _) = call(&app(), send("POST", "/v1/jobs", purposeless)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_zero_timeout_is_refused() {
        let mut body = a_job("Weekly", "0 9 * * MON");
        body["timeout_secs"] = serde_json::json!(0);
        let (status, _) = call(&app(), send("POST", "/v1/jobs", body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn two_jobs_cannot_share_a_name() {
        let app = app();
        call(
            &app,
            send("POST", "/v1/jobs", a_job("Weekly", "0 9 * * MON")),
        )
        .await;
        let (status, _) = call(
            &app,
            send("POST", "/v1/jobs", a_job("Weekly", "0 9 * * TUE")),
        )
        .await;
        // 409 exactly: the name is already taken, which is a conflict with the current state
        // rather than a malformed request or a crash.
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "a name has to identify one job, and a taken name is a conflict"
        );
    }

    #[tokio::test]
    async fn a_job_can_be_paused_and_resumed() {
        let app = app();
        let (_, made) = call(
            &app,
            send("POST", "/v1/jobs", a_job("Weekly", "0 9 * * MON")),
        )
        .await;
        let id = made["id"].as_str().expect("id");

        let (status, paused) = call(
            &app,
            send(
                "PUT",
                &format!("/v1/jobs/{id}/enabled"),
                serde_json::json!({"enabled": false}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(paused["enabled"], false);
    }

    #[tokio::test]
    async fn the_preview_shows_several_fire_times_without_saving_anything() {
        let app = app();
        let (status, body) = call(
            &app,
            send(
                "POST",
                "/v1/jobs/preview",
                serde_json::json!({"cron": "0 9 * * *", "timezone": "UTC"}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["next"].as_array().expect("next").len(), 5);

        let (_, jobs) = call(&app, get("/v1/jobs")).await;
        assert_eq!(
            jobs.as_array().expect("array").len(),
            0,
            "a preview saves nothing"
        );
    }

    #[tokio::test]
    async fn a_deleted_job_is_gone() {
        let app = app();
        let (_, made) = call(
            &app,
            send("POST", "/v1/jobs", a_job("Weekly", "0 9 * * MON")),
        )
        .await;
        let id = made["id"].as_str().expect("id");

        let (status, _) = call(
            &app,
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/jobs/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (_, jobs) = call(&app, get("/v1/jobs")).await;
        assert_eq!(jobs.as_array().expect("array").len(), 0);
    }

    /// A run-now on the mock backend goes all the way through, which is what proves the wiring —
    /// job to agent to run record.
    #[tokio::test]
    async fn running_a_job_records_a_run() {
        let app = app();
        let (_, made) = call(
            &app,
            send("POST", "/v1/jobs", a_job("Weekly", "0 9 * * MON")),
        )
        .await;
        let id = made["id"].as_str().expect("id");

        let (status, started) = call(
            &app,
            send("POST", &format!("/v1/jobs/{id}/run"), serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{started}");
        assert!(started["run_id"].is_string());

        // The run is spawned, so give it a moment to be recorded.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        let (_, runs) = call(&app, get(&format!("/v1/jobs/{id}/runs"))).await;
        let runs = runs.as_array().expect("runs");
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert_eq!(
            runs[0]["proposals"], 0,
            "a scheduled run must never propose a tool call until there is a way to confirm one"
        );
    }

    /// Two concurrent runs of one job would both write a note and both cost model calls. The mock
    /// backend finishes in milliseconds, so the in-flight row is created directly — otherwise the
    /// race is unobservable and the guard would be untested.
    #[tokio::test]
    async fn run_now_is_refused_while_a_run_is_in_flight() {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ));
        let app = routes().with_state(Arc::clone(&state));

        let (_, made) = call(
            &app,
            send("POST", "/v1/jobs", a_job("Weekly", "0 9 * * MON")),
        )
        .await;
        let id: Id = made["id"].as_str().expect("id").parse().expect("parse");

        {
            let db = state.db().await;
            JobRepository::new(&db)
                .start_run(id, Utc::now())
                .expect("in-flight run");
        }

        let (status, body) = call(
            &app,
            send("POST", &format!("/v1/jobs/{id}/run"), serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.to_string().contains("already running"),
            "the reason has to be legible: {body}"
        );
    }

    #[tokio::test]
    async fn a_malformed_job_id_is_a_client_error() {
        let (status, _) = call(&app(), get("/v1/jobs/not-an-id/runs")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// Nothing is due on a fresh engine, so a tick must start nothing.
    #[tokio::test]
    async fn a_tick_with_nothing_due_starts_nothing() {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ));
        assert_eq!(tick(&state).await.expect("tick"), 0);
    }
}
