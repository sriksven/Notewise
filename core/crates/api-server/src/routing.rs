//! Reading, writing and explaining the model routing policy.
//!
//! # Why this is a separate module
//!
//! `routes.rs` is already several thousand lines. Routing has its own validation rules, its own
//! failure modes, and one endpoint whose whole purpose is to be readable by a human — it earns a
//! file.
//!
//! # Why the explain endpoint exists
//!
//! Routing spends money on the user's behalf without being asked each time. "Why did that summary
//! cost anything" has to be answerable, or the honest response to a surprising bill is to turn
//! routing off. [`explain`] answers it for a hypothetical request, before any call is made.

use std::sync::Arc;

use axum::extract::State;
use axum::{
    routing::{get, post},
    Json, Router as AxumRouter,
};
use notewise_ai_router::{
    contradictory_route, unreachable_route, BackendKind, Predicate, RequestFacts, RouteSpec,
    StoredRoute, TaskKind,
};
use notewise_storage::SettingsRepository;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, ROUTING_RULES_KEY};

type Shared = Arc<AppState>;

pub fn routes() -> AxumRouter<Shared> {
    AxumRouter::new()
        .route("/v1/routing/rules", get(get_rules).put(put_rules))
        .route("/v1/routing/explain", post(explain))
        .route("/v1/routing/default", post(install_default))
}

#[derive(Debug, Serialize)]
struct RulesResponse {
    rules: Vec<StoredRoute>,
    /// Names in evaluation order, as the *running* router holds them. A rule that failed to build
    /// is stored but absent here, which is how a user sees that one is not in force.
    active: Vec<String>,
}

/// The stored rule set, and which of them the running router actually built.
///
/// Reporting both matters: a rule whose backend could not be constructed is skipped at load with a
/// warning, and a settings page showing only the stored list would present it as working.
async fn get_rules(State(state): State<Shared>) -> ApiResult<Json<RulesResponse>> {
    let rules = {
        let db = state.db().await;
        crate::state::stored_routes(&db)
    };

    Ok(Json(RulesResponse {
        rules,
        active: state.ai().route_names(),
    }))
}

#[derive(Debug, Deserialize)]
struct PutRulesBody {
    rules: Vec<StoredRoute>,
}

/// Replace the rule set.
///
/// Validated before it is stored, and the validation is the point. Two mistakes are easy to make
/// and impossible to notice afterwards, because both produce a rule that is listed and never runs:
/// a rule below a catch-all, and a rule whose bounds contradict each other. Rejecting them here
/// means the failure arrives while the user is looking at the rule they just wrote.
async fn put_rules(
    State(state): State<Shared>,
    Json(body): Json<PutRulesBody>,
) -> ApiResult<Json<RulesResponse>> {
    let specs: Vec<RouteSpec> = body.rules.iter().map(|r| r.spec.clone()).collect();

    for (i, rule) in body.rules.iter().enumerate() {
        if rule.spec.name.trim().is_empty() {
            return Err(ApiError::BadRequest(format!(
                "rule {i} has no name; a rule you cannot refer to is a rule you cannot debug"
            )));
        }
        if !rule.backend.is_selectable() {
            return Err(ApiError::BadRequest(format!(
                "rule '{}' targets '{}', which does not run a model and would return invented \
                 answers",
                rule.spec.name,
                rule.backend.as_str()
            )));
        }
        if rule.backend.requires_endpoint() && rule.endpoint.is_none() {
            return Err(ApiError::BadRequest(format!(
                "rule '{}' targets {} and needs an endpoint URL",
                rule.spec.name,
                rule.backend.label()
            )));
        }
    }

    if let Some(i) = unreachable_route(&specs) {
        return Err(ApiError::BadRequest(format!(
            "rule '{}' can never run: an earlier rule matches every request. Move it above that \
             rule, or give that rule a condition.",
            specs[i].name
        )));
    }

    if let Some(i) = contradictory_route(&specs) {
        return Err(ApiError::BadRequest(format!(
            "rule '{}' can never match: its size bounds cannot both hold",
            specs[i].name
        )));
    }

    let encoded = serde_json::to_string(&body.rules)
        .map_err(|e| ApiError::Internal(format!("could not encode the rules: {e}")))?;

    {
        let db = state.db().await;
        SettingsRepository::new(&db)
            .set(ROUTING_RULES_KEY, &encoded)
            .map_err(|e| ApiError::Internal(format!("could not save the rules: {e}")))?;
    }

    // Rebuild the live router so the change applies without a restart. Reusing the current kind
    // and model means this is a policy change and never a silent backend change.
    let ai = state.ai();
    state
        .switch_backend(ai.kind(), Some(ai.model_id().to_string()), None)
        .await?;

    Ok(Json(RulesResponse {
        rules: body.rules,
        active: state.ai().route_names(),
    }))
}

#[derive(Debug, Deserialize)]
struct ExplainBody {
    /// Which kind of work to simulate. Defaults to a summary, the expensive case.
    #[serde(default)]
    task: Option<String>,
    /// Roughly how large the input is. Defaults to something small.
    #[serde(default)]
    estimated_tokens: Option<usize>,
    /// Title or question text, for keyword rules.
    #[serde(default)]
    text: Option<String>,
    /// Local hour to simulate, 0..=23. Defaults to now.
    #[serde(default)]
    hour_of_day: Option<u8>,
}

#[derive(Debug, Serialize)]
struct ExplainResponse {
    /// Human-readable: which rule matched and which provider it reaches.
    decision: String,
    /// The facts the decision was made from, echoed so a surprising answer is debuggable.
    task: String,
    estimated_tokens: usize,
    hour_of_day: u8,
}

/// Where a request with these characteristics would go, and why.
///
/// A dry run. Nothing is sent to any provider — the whole point is to answer the cost question
/// without incurring the cost.
async fn explain(
    State(state): State<Shared>,
    Json(body): Json<ExplainBody>,
) -> ApiResult<Json<ExplainResponse>> {
    let task = match body.task.as_deref().map(str::trim) {
        None | Some("") => TaskKind::Summarize,
        Some(name) => TaskKind::parse(name).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "unknown task '{name}'; expected one of summarize, extract_decisions, \
                 extract_action_items, chat"
            ))
        })?,
    };

    if let Some(hour) = body.hour_of_day {
        if hour > 23 {
            return Err(ApiError::BadRequest(format!(
                "hour_of_day must be 0..=23, got {hour}"
            )));
        }
    }

    let text = body.text.unwrap_or_default();
    let hour_of_day = body.hour_of_day.unwrap_or_else(current_hour);
    let facts = RequestFacts {
        task,
        estimated_tokens: body.estimated_tokens.unwrap_or(0),
        hour_of_day,
        text: text.to_lowercase(),
    };

    Ok(Json(ExplainResponse {
        decision: state.ai().explain(&facts),
        task: task.as_str().to_string(),
        estimated_tokens: facts.estimated_tokens,
        hour_of_day,
    }))
}

fn current_hour() -> u8 {
    use chrono::Timelike;
    chrono::Local::now().hour() as u8
}

#[derive(Debug, Deserialize)]
struct InstallDefaultBody {
    /// The backend to send heavy work to. Must be a real, selectable backend.
    quality_backend: String,
    #[serde(default)]
    quality_model: Option<String>,
}

/// Install the two-rule starting policy: heavy work to a chosen backend, everything else local.
///
/// Offered as an action rather than seeded automatically. Seeding it at first launch would mean a
/// fresh install silently acquiring a rule that sends transcripts to a provider the user has not
/// chosen — and with no cloud backend configured the rules would collapse to local anyway, so it
/// would be a no-op that only confuses the settings page.
async fn install_default(
    State(state): State<Shared>,
    Json(body): Json<InstallDefaultBody>,
) -> ApiResult<Json<RulesResponse>> {
    let backend = BackendKind::parse(body.quality_backend.trim()).ok_or_else(|| {
        ApiError::BadRequest(format!("unknown backend '{}'", body.quality_backend))
    })?;

    if !backend.is_selectable() {
        return Err(ApiError::BadRequest(format!(
            "'{}' does not run a model",
            backend.as_str()
        )));
    }

    let rules = vec![StoredRoute {
        spec: RouteSpec {
            name: "Heavy work".into(),
            when: vec![Predicate::Task(vec![TaskKind::Summarize])],
        },
        backend,
        model: body.quality_model,
        endpoint: None,
        redaction: Default::default(),
    }];

    // One rule, not two. The spec described "summaries to quality, everything else local", and the
    // second half is what the default backend already does — expressing it as a catch-all rule
    // would add something that can never change the outcome and would make every later rule
    // unreachable.
    put_rules(State(state), Json(PutRulesBody { rules })).await
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

    fn rule(name: &str, when: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "name": name, "when": when, "backend": "ollama" })
    }

    #[tokio::test]
    async fn a_fresh_engine_has_no_rules() {
        let (status, body) = call(&app(), get("/v1/routing/rules")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["rules"].as_array().expect("rules").len(), 0);
        assert_eq!(body["active"].as_array().expect("active").len(), 0);
    }

    #[tokio::test]
    async fn rules_round_trip_and_become_active() {
        let app = app();
        let rules = serde_json::json!({
            "rules": [rule("summaries", serde_json::json!([{ "task": ["summarize"] }]))]
        });

        let (status, body) = call(&app, send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["active"][0], "summaries");

        let (_, reread) = call(&app, get("/v1/routing/rules")).await;
        assert_eq!(reread["rules"][0]["name"], "summaries");
    }

    /// The two mistakes that produce a rule which is listed and never runs. Both have to be caught
    /// while the user is still looking at the rule they wrote.
    #[tokio::test]
    async fn a_rule_below_a_catch_all_is_refused() {
        let rules = serde_json::json!({
            "rules": [
                rule("everything", serde_json::json!([])),
                rule("never runs", serde_json::json!([{ "task": ["chat"] }])),
            ]
        });

        let (status, body) = call(&app(), send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body.to_string().contains("never runs"),
            "the error must name the dead rule: {body}"
        );
    }

    #[tokio::test]
    async fn a_rule_with_impossible_bounds_is_refused() {
        let rules = serde_json::json!({
            "rules": [rule(
                "impossible",
                serde_json::json!([
                    { "input_tokens_over": 1000 },
                    { "input_tokens_under": 100 },
                ])
            )]
        });

        let (status, body) = call(&app(), send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    #[tokio::test]
    async fn an_unnamed_rule_is_refused() {
        let rules = serde_json::json!({ "rules": [rule("   ", serde_json::json!([]))] });
        let (status, _) = call(&app(), send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// The mock backend answers every request with fixed text. Routing to it would produce
    /// invented summaries of a real meeting, presented exactly like real ones.
    #[tokio::test]
    async fn a_rule_targeting_the_mock_backend_is_refused() {
        let rules = serde_json::json!({
            "rules": [{ "name": "fake", "when": [], "backend": "mock" }]
        });
        let (status, body) = call(&app(), send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    #[tokio::test]
    async fn a_custom_endpoint_backend_without_a_url_is_refused() {
        let rules = serde_json::json!({
            "rules": [{ "name": "custom", "when": [], "backend": "openai_compatible" }]
        });
        let (status, body) = call(&app(), send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    #[tokio::test]
    async fn explain_names_the_default_when_nothing_matches() {
        let (status, body) = call(
            &app(),
            send("POST", "/v1/routing/explain", serde_json::json!({})),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(
            body["decision"]
                .as_str()
                .expect("decision")
                .contains("default"),
            "{body}"
        );
        assert_eq!(body["task"], "summarize", "a summary is the expensive case");
    }

    #[tokio::test]
    async fn explain_names_the_rule_that_would_match() {
        let app = app();
        let rules = serde_json::json!({
            "rules": [rule("big ones", serde_json::json!([{ "input_tokens_over": 100 }]))]
        });
        let (status, _) = call(&app, send("PUT", "/v1/routing/rules", rules)).await;
        assert_eq!(status, StatusCode::OK);

        let (_, matched) = call(
            &app,
            send(
                "POST",
                "/v1/routing/explain",
                serde_json::json!({ "estimated_tokens": 5000 }),
            ),
        )
        .await;
        assert!(
            matched["decision"]
                .as_str()
                .expect("decision")
                .contains("big ones"),
            "{matched}"
        );

        let (_, small) = call(
            &app,
            send(
                "POST",
                "/v1/routing/explain",
                serde_json::json!({ "estimated_tokens": 10 }),
            ),
        )
        .await;
        assert!(
            small["decision"]
                .as_str()
                .expect("decision")
                .contains("default"),
            "{small}"
        );
    }

    #[tokio::test]
    async fn explain_rejects_an_unknown_task_and_an_impossible_hour() {
        let app = app();

        let (status, _) = call(
            &app,
            send(
                "POST",
                "/v1/routing/explain",
                serde_json::json!({ "task": "transcribe" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = call(
            &app,
            send(
                "POST",
                "/v1/routing/explain",
                serde_json::json!({ "hour_of_day": 25 }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_default_policy_installs_one_rule_for_heavy_work() {
        let (status, body) = call(
            &app(),
            send(
                "POST",
                "/v1/routing/default",
                serde_json::json!({ "quality_backend": "ollama" }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["rules"].as_array().expect("rules").len(), 1);
        assert_eq!(body["active"][0], "Heavy work");
    }

    #[tokio::test]
    async fn the_default_policy_refuses_a_backend_that_runs_no_model() {
        let (status, _) = call(
            &app(),
            send(
                "POST",
                "/v1/routing/default",
                serde_json::json!({ "quality_backend": "mock" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
