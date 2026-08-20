//! Noticing that a meeting has started, and offering to record it.
//!
//! # Why the decision lives in one pure object
//!
//! Signals arrive from different places at different times and each is individually unreliable. A
//! tab being open is not a meeting; a calendar entry is not attendance. Fusing them in one place
//! means the rules about *when to speak* are one testable object rather than scattered across the
//! things that produce signals.
//!
//! This is the shape `clarify.rs` already uses, for the reason it states: almost all the difficulty
//! is in *when* to speak rather than what to say.
//!
//! # Why detection never starts a recording
//!
//! It creates a notification and waits to be clicked. Recording on a guess would mean capturing
//! audio of other people because software inferred a meeting began — and a false positive there is a
//! recording nobody knew about, which is a different category of failure from a missed meeting. One
//! is lost value; the other is a breach of the thing the product sells. The asymmetry decides it.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Where a signal came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalSource {
    /// The browser extension, watching a meeting page.
    Extension,
    /// A calendar event that is happening now.
    Calendar,
    /// A native audio-session check. Not implemented; the variant exists so fusion does not have to
    /// change when it is.
    Native,
}

/// Which meeting platform, when it is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Meet,
    Zoom,
    Teams,
    Unknown,
}

impl Platform {
    /// Recognise a platform from a meeting URL.
    pub fn from_url(url: &str) -> Self {
        let lower = url.to_ascii_lowercase();
        if lower.contains("meet.google.com") {
            Platform::Meet
        } else if lower.contains("zoom.us") {
            Platform::Zoom
        } else if lower.contains("teams.microsoft.com") || lower.contains("teams.live.com") {
            Platform::Teams
        } else {
            Platform::Unknown
        }
    }
}

/// How much a signal is worth on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Suggestive. A calendar full of meetings nobody attends is the reason this exists.
    Weak,
    /// Deliberate. Somebody has a meeting page open.
    Strong,
}

/// One observation that a meeting might be happening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinSignal {
    pub source: SignalSource,
    pub platform: Option<Platform>,
    pub confidence: Confidence,
    /// A calendar external id or a meeting URL, whichever the producer has.
    pub external_ref: Option<String>,
    /// Whether the user declined this. A declined event never produces a notification.
    pub declined: bool,
    /// A title to put in the prompt, so a recording starts labelled rather than as "Meeting".
    pub title: Option<String>,
    pub observed_at: DateTime<Utc>,
}

/// What the tracker decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Tell the user now.
    Notify {
        key: String,
        title: Option<String>,
        platform: Option<Platform>,
    },
    /// A weak signal on its own. Ask again after this.
    Waiting { until: DateTime<Utc> },
    /// Deliberately silent, and why.
    Silent { reason: &'static str },
}

impl Decision {
    pub fn notifies(&self) -> bool {
        matches!(self, Decision::Notify { .. })
    }
}

/// Tuning.
#[derive(Debug, Clone, Copy)]
pub struct TrackerConfig {
    /// How long a lone weak signal waits before it is worth mentioning.
    ///
    /// Calendars are full of meetings nobody attends. Waiting converts "your calendar says
    /// something" into "something appears to be happening".
    pub grace: Duration,
    /// How long one meeting stays deduplicated.
    ///
    /// Repeats are what kills this feature: three notifications for one standup and it gets turned
    /// off, permanently.
    pub dedup_window: Duration,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            grace: Duration::minutes(2),
            dedup_window: Duration::hours(2),
        }
    }
}

/// Fuses signals and decides when to speak.
#[derive(Debug)]
pub struct JoinTracker {
    config: TrackerConfig,
    /// Weak signals seen but not yet acted on, by key.
    pending: HashMap<String, Pending>,
    /// Keys already notified, and when.
    notified: HashMap<String, DateTime<Utc>>,
}

#[derive(Debug, Clone)]
struct Pending {
    first_seen: DateTime<Utc>,
    /// Distinct sources that have reported this key. Two weak sources agreeing is as good as one
    /// strong one — they are independent observations of the same thing.
    sources: Vec<SignalSource>,
    title: Option<String>,
    platform: Option<Platform>,
}

impl Default for JoinTracker {
    fn default() -> Self {
        Self::new(TrackerConfig::default())
    }
}

impl JoinTracker {
    pub fn new(config: TrackerConfig) -> Self {
        Self {
            config,
            pending: HashMap::new(),
            notified: HashMap::new(),
        }
    }

    /// Take a signal and decide.
    pub fn observe(&mut self, signal: &JoinSignal, now: DateTime<Utc>) -> Decision {
        // Checked first: notifying about a meeting somebody explicitly declined is the most
        // annoying false positive available.
        if signal.declined {
            return Decision::Silent {
                reason: "the user declined this meeting",
            };
        }

        let key = dedup_key(signal);

        if let Some(at) = self.notified.get(&key) {
            if now - *at < self.config.dedup_window {
                return Decision::Silent {
                    reason: "already mentioned this meeting",
                };
            }
        }

        // A meeting page open is deliberate — somebody navigated there. No waiting.
        if signal.confidence == Confidence::Strong {
            self.notified.insert(key.clone(), now);
            self.pending.remove(&key);
            return Decision::Notify {
                key,
                title: signal.title.clone(),
                platform: signal.platform,
            };
        }

        let entry = self.pending.entry(key.clone()).or_insert_with(|| Pending {
            first_seen: signal.observed_at,
            sources: Vec::new(),
            title: signal.title.clone(),
            platform: signal.platform,
        });
        if !entry.sources.contains(&signal.source) {
            entry.sources.push(signal.source);
        }
        // A later signal may know more than the first — a calendar poll that runs after the
        // extension has a title where the extension had none.
        if entry.title.is_none() {
            entry.title = signal.title.clone();
        }
        if entry.platform.is_none() {
            entry.platform = signal.platform;
        }

        let corroborated = entry.sources.len() >= 2;
        let waited_long_enough = now - entry.first_seen >= self.config.grace;

        if corroborated || waited_long_enough {
            let pending = self.pending.remove(&key).expect("just inserted");
            self.notified.insert(key.clone(), now);
            return Decision::Notify {
                key,
                title: pending.title,
                platform: pending.platform,
            };
        }

        Decision::Waiting {
            until: entry.first_seen + self.config.grace,
        }
    }

    /// Forget state older than the dedup window.
    ///
    /// Called periodically so a long-running process does not accumulate a key per meeting forever.
    pub fn prune(&mut self, now: DateTime<Utc>) {
        let window = self.config.dedup_window;
        self.notified.retain(|_, at| now - *at < window);
        self.pending.retain(|_, p| now - p.first_seen < window);
    }

    pub fn tracked(&self) -> usize {
        self.pending.len() + self.notified.len()
    }
}

/// The identity of a meeting occurrence, from the strongest thing available.
///
/// Calendar id first, then the normalised URL, then platform plus a coarse time bucket. The bucket
/// exists so two signals minutes apart about the same untitled Zoom call collapse — without it, a
/// meeting with no identifiable reference would notify on every poll.
pub fn dedup_key(signal: &JoinSignal) -> String {
    if let Some(reference) = signal.external_ref.as_deref() {
        let trimmed = reference.trim();
        if !trimmed.is_empty() {
            return if trimmed.starts_with("http") {
                format!("url:{}", normalize_url(trimmed))
            } else {
                format!("cal:{trimmed}")
            };
        }
    }

    // Fifteen-minute buckets: long enough that signals about one meeting collapse, short enough
    // that back-to-back calls do not.
    let bucket = signal.observed_at.timestamp() / (15 * 60);
    let platform = signal
        .platform
        .map(|p| format!("{p:?}"))
        .unwrap_or_else(|| "unknown".into());
    format!("time:{platform}:{bucket}")
}

/// Reduce a meeting URL to the part that identifies the meeting.
///
/// Query strings carry per-participant tokens, so two people's links to one call differ. Comparing
/// them raw would treat the same meeting as two.
pub fn normalize_url(url: &str) -> String {
    let lower = url.trim().to_ascii_lowercase();
    let without_scheme = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or(&lower);
    let without_query = without_scheme
        .split(['?', '#'])
        .next()
        .unwrap_or(without_scheme);
    without_query.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(mins: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + mins * 60, 0).expect("timestamp")
    }

    fn signal(source: SignalSource, confidence: Confidence, at: DateTime<Utc>) -> JoinSignal {
        JoinSignal {
            source,
            platform: Some(Platform::Meet),
            confidence,
            external_ref: Some("cal-evt-1".into()),
            declined: false,
            title: Some("Platform standup".into()),
            observed_at: at,
        }
    }

    /// Somebody navigated to a meeting page. That is deliberate and needs no corroboration.
    #[test]
    fn a_strong_signal_notifies_at_once() {
        let mut tracker = JoinTracker::default();
        let decision = tracker.observe(
            &signal(SignalSource::Extension, Confidence::Strong, at(0)),
            at(0),
        );

        assert!(decision.notifies(), "{decision:?}");
        match decision {
            Decision::Notify { title, .. } => {
                assert_eq!(title.as_deref(), Some("Platform standup"))
            }
            other => panic!("{other:?}"),
        }
    }

    /// Calendars are full of meetings nobody attends.
    #[test]
    fn a_lone_weak_signal_waits_then_speaks() {
        let mut tracker = JoinTracker::default();
        let first = tracker.observe(
            &signal(SignalSource::Calendar, Confidence::Weak, at(0)),
            at(0),
        );
        assert!(matches!(first, Decision::Waiting { .. }), "{first:?}");

        // Same signal seen again after the grace period.
        let later = tracker.observe(
            &signal(SignalSource::Calendar, Confidence::Weak, at(0)),
            at(3),
        );
        assert!(later.notifies(), "{later:?}");
    }

    /// Two independent observations of the same thing are as good as one deliberate one.
    #[test]
    fn two_weak_sources_corroborate_without_waiting() {
        let mut tracker = JoinTracker::default();
        tracker.observe(
            &signal(SignalSource::Calendar, Confidence::Weak, at(0)),
            at(0),
        );

        let second = tracker.observe(
            &signal(SignalSource::Native, Confidence::Weak, at(0)),
            at(0),
        );
        assert!(second.notifies(), "{second:?}");
    }

    /// The same source reporting twice is one observation, not two.
    #[test]
    fn the_same_weak_source_twice_is_not_corroboration() {
        let mut tracker = JoinTracker::default();
        tracker.observe(
            &signal(SignalSource::Calendar, Confidence::Weak, at(0)),
            at(0),
        );
        let again = tracker.observe(
            &signal(SignalSource::Calendar, Confidence::Weak, at(0)),
            at(1),
        );

        assert!(
            matches!(again, Decision::Waiting { .. }),
            "one source agreeing with itself proves nothing: {again:?}"
        );
    }

    /// Three notifications for one standup and the feature gets turned off, permanently.
    #[test]
    fn one_meeting_is_mentioned_once() {
        let mut tracker = JoinTracker::default();
        assert!(tracker
            .observe(
                &signal(SignalSource::Extension, Confidence::Strong, at(0)),
                at(0)
            )
            .notifies());

        for minute in [1, 5, 30, 100] {
            let again = tracker.observe(
                &signal(SignalSource::Extension, Confidence::Strong, at(minute)),
                at(minute),
            );
            assert!(!again.notifies(), "at {minute}m: {again:?}");
        }
    }

    #[test]
    fn a_meeting_can_be_mentioned_again_after_the_window() {
        let mut tracker = JoinTracker::default();
        tracker.observe(
            &signal(SignalSource::Extension, Confidence::Strong, at(0)),
            at(0),
        );

        // A weekly standup with the same calendar id, a week later.
        let next_week = tracker.observe(
            &signal(SignalSource::Extension, Confidence::Strong, at(60 * 24 * 7)),
            at(60 * 24 * 7),
        );
        assert!(next_week.notifies(), "{next_week:?}");
    }

    /// The most annoying false positive available.
    #[test]
    fn a_declined_meeting_is_never_mentioned() {
        let mut tracker = JoinTracker::default();
        let mut declined = signal(SignalSource::Extension, Confidence::Strong, at(0));
        declined.declined = true;

        let decision = tracker.observe(&declined, at(0));
        assert!(matches!(decision, Decision::Silent { .. }), "{decision:?}");
    }

    /// Two people's links to one call differ by a per-participant token.
    #[test]
    fn urls_are_normalised_before_comparing() {
        assert_eq!(
            normalize_url("https://meet.google.com/abc-defg-hij?authuser=1"),
            normalize_url("http://meet.google.com/abc-defg-hij/")
        );
        assert_eq!(
            normalize_url("https://Zoom.us/j/123#success"),
            "zoom.us/j/123"
        );
    }

    #[test]
    fn a_calendar_id_and_a_url_produce_different_keys() {
        let mut by_id = signal(SignalSource::Calendar, Confidence::Weak, at(0));
        by_id.external_ref = Some("evt-1".into());

        let mut by_url = signal(SignalSource::Extension, Confidence::Strong, at(0));
        by_url.external_ref = Some("https://meet.google.com/abc".into());

        assert_ne!(dedup_key(&by_id), dedup_key(&by_url));
        assert!(dedup_key(&by_id).starts_with("cal:"));
        assert!(dedup_key(&by_url).starts_with("url:"));
    }

    /// Without a bucket, a meeting with no identifiable reference notifies on every poll.
    #[test]
    fn signals_with_no_reference_collapse_within_a_bucket() {
        let mut anonymous = signal(SignalSource::Extension, Confidence::Strong, at(0));
        anonymous.external_ref = None;

        let mut minutes_later = anonymous.clone();
        minutes_later.observed_at = at(3);

        assert_eq!(dedup_key(&anonymous), dedup_key(&minutes_later));

        let mut much_later = anonymous.clone();
        much_later.observed_at = at(40);
        assert_ne!(dedup_key(&anonymous), dedup_key(&much_later));
    }

    #[test]
    fn a_later_signal_can_supply_a_title_the_first_lacked() {
        let mut tracker = JoinTracker::default();
        let mut untitled = signal(SignalSource::Extension, Confidence::Weak, at(0));
        untitled.title = None;
        untitled.platform = None;
        tracker.observe(&untitled, at(0));

        let titled = signal(SignalSource::Calendar, Confidence::Weak, at(0));
        match tracker.observe(&titled, at(0)) {
            Decision::Notify {
                title, platform, ..
            } => {
                assert_eq!(title.as_deref(), Some("Platform standup"));
                assert_eq!(platform, Some(Platform::Meet));
            }
            other => panic!("expected corroboration, got {other:?}"),
        }
    }

    #[test]
    fn platforms_are_recognised_from_their_urls() {
        assert_eq!(
            Platform::from_url("https://meet.google.com/x"),
            Platform::Meet
        );
        assert_eq!(
            Platform::from_url("https://acme.zoom.us/j/1"),
            Platform::Zoom
        );
        assert_eq!(
            Platform::from_url("https://teams.microsoft.com/l/x"),
            Platform::Teams
        );
        assert_eq!(
            Platform::from_url("https://example.com/call"),
            Platform::Unknown
        );
    }

    /// A long-running process must not accumulate a key per meeting forever.
    #[test]
    fn state_is_pruned_after_the_window() {
        let mut tracker = JoinTracker::default();
        tracker.observe(
            &signal(SignalSource::Extension, Confidence::Strong, at(0)),
            at(0),
        );
        assert_eq!(tracker.tracked(), 1);

        tracker.prune(at(60 * 24));
        assert_eq!(tracker.tracked(), 0);
    }

    #[test]
    fn pruning_keeps_what_is_still_current() {
        let mut tracker = JoinTracker::default();
        tracker.observe(
            &signal(SignalSource::Extension, Confidence::Strong, at(0)),
            at(0),
        );

        tracker.prune(at(5));
        assert_eq!(tracker.tracked(), 1, "still inside the dedup window");
    }
}

// ---------------------------------------------------------------- HTTP surface

use std::sync::Arc;

use axum::extract::State;
use axum::{routing::post, Json, Router as AxumRouter};
use notewise_storage::{Id, NewNotification, NotificationChannel, NotificationRepository};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

type Shared = Arc<AppState>;

pub fn routes() -> AxumRouter<Shared> {
    AxumRouter::new().route("/v1/signals/join", post(report_signal))
}

#[derive(Debug, Deserialize)]
struct SignalBody {
    /// `extension`, `calendar`, or `native`.
    source: String,
    /// A meeting URL. The platform and the dedup key are both derived from it.
    #[serde(default)]
    url: Option<String>,
    /// A calendar event's external id, when the producer has one instead of a URL.
    #[serde(default)]
    external_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    declined: bool,
    /// Whether this is a deliberate observation. Defaults to weak, so a producer that forgets to
    /// say gets the cautious treatment rather than an immediate notification.
    #[serde(default)]
    strong: bool,
}

#[derive(Debug, Serialize)]
struct SignalResponse {
    /// `notified`, `waiting`, or `silent`.
    outcome: &'static str,
    /// Why, when it stayed quiet. The extension logs this, which is what makes the rules debuggable
    /// from outside.
    reason: Option<String>,
    notification_id: Option<String>,
}

/// Report that a meeting may have started.
///
/// Never starts a recording. It queues a notification the shell delivers and the user clicks — see
/// the module docs for why a false positive here must not be a recording nobody knew about.
async fn report_signal(
    State(state): State<Shared>,
    Json(body): Json<SignalBody>,
) -> ApiResult<Json<SignalResponse>> {
    let source = match body.source.trim() {
        "extension" => SignalSource::Extension,
        "calendar" => SignalSource::Calendar,
        "native" => SignalSource::Native,
        other => {
            return Err(ApiError::BadRequest(format!(
                "'{other}' is not a signal source; expected extension, calendar, or native"
            )))
        }
    };

    let platform = body.url.as_deref().map(Platform::from_url);
    let signal = JoinSignal {
        source,
        platform,
        confidence: if body.strong {
            Confidence::Strong
        } else {
            Confidence::Weak
        },
        // A URL identifies a meeting more precisely than a calendar id when both are present, but
        // the calendar id survives a link being regenerated — so it wins.
        external_ref: body.external_id.clone().or_else(|| body.url.clone()),
        declined: body.declined,
        title: body.title.clone(),
        observed_at: Utc::now(),
    };

    let decision = state.join_tracker().await.observe(&signal, Utc::now());

    match decision {
        Decision::Notify {
            title, platform, ..
        } => {
            let described = title.unwrap_or_else(|| match platform {
                Some(Platform::Meet) => "A Google Meet call".into(),
                Some(Platform::Zoom) => "A Zoom call".into(),
                Some(Platform::Teams) => "A Teams call".into(),
                _ => "A meeting".into(),
            });

            let db = state.db().await;
            let queued = NotificationRepository::new(&db).create(NewNotification {
                source_kind: "meeting".into(),
                // No meeting exists yet — that is the point, the user is being asked whether to
                // make one. A fresh id keeps the row well-formed without implying a link.
                source_id: Id::new(),
                recipient: "me".into(),
                channel: NotificationChannel::Desktop,
                body: format!("{described} seems to have started. Record it?"),
            })?;

            Ok(Json(SignalResponse {
                outcome: "notified",
                reason: None,
                notification_id: Some(queued.id.to_string()),
            }))
        }
        Decision::Waiting { until } => Ok(Json(SignalResponse {
            outcome: "waiting",
            reason: Some(format!("a single weak signal; will decide after {until}")),
            notification_id: None,
        })),
        Decision::Silent { reason } => Ok(Json(SignalResponse {
            outcome: "silent",
            reason: Some(reason.to_string()),
            notification_id: None,
        })),
    }
}

#[cfg(test)]
mod http_tests {
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

    async fn post_signal(
        app: &AxumRouter<()>,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/signals/join")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request");

        let response = app.clone().oneshot(request).await.expect("call");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn a_strong_extension_signal_queues_a_notification() {
        let (status, body) = post_signal(
            &app(),
            serde_json::json!({
                "source": "extension",
                "url": "https://meet.google.com/abc-defg-hij",
                "title": "Platform standup",
                "strong": true
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["outcome"], "notified");
        assert!(body["notification_id"].is_string());
    }

    #[tokio::test]
    async fn a_lone_calendar_signal_waits_and_says_so() {
        let (status, body) = post_signal(
            &app(),
            serde_json::json!({ "source": "calendar", "external_id": "evt-1" }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["outcome"], "waiting");
        assert!(
            body["reason"].is_string(),
            "the extension logs this, which is what makes the rules debuggable from outside"
        );
    }

    #[tokio::test]
    async fn a_declined_meeting_stays_silent() {
        let (_, body) = post_signal(
            &app(),
            serde_json::json!({
                "source": "extension",
                "external_id": "evt-1",
                "declined": true,
                "strong": true
            }),
        )
        .await;

        assert_eq!(body["outcome"], "silent");
        assert!(body["reason"].as_str().unwrap().contains("declined"));
    }

    #[tokio::test]
    async fn the_same_meeting_is_not_queued_twice() {
        let app = app();
        let payload = serde_json::json!({
            "source": "extension",
            "url": "https://meet.google.com/abc",
            "strong": true
        });

        let (_, first) = post_signal(&app, payload.clone()).await;
        let (_, second) = post_signal(&app, payload).await;

        assert_eq!(first["outcome"], "notified");
        assert_eq!(
            second["outcome"], "silent",
            "three notifications for one standup and the feature gets turned off"
        );
    }

    /// A producer that forgets to say gets the cautious treatment.
    #[tokio::test]
    async fn a_signal_that_does_not_claim_strength_is_treated_as_weak() {
        let (_, body) = post_signal(
            &app(),
            serde_json::json!({ "source": "extension", "external_id": "evt-1" }),
        )
        .await;

        assert_eq!(body["outcome"], "waiting");
    }

    #[tokio::test]
    async fn an_unknown_source_is_refused() {
        let (status, _) = post_signal(
            &app(),
            serde_json::json!({ "source": "telepathy", "strong": true }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_platform_is_named_when_there_is_no_title() {
        let state = Arc::new(AppState::new(
            Database::open_in_memory().expect("in-memory db"),
            AiRouter::from_config(RouterConfig::mock()).expect("mock router"),
        ));
        let app = routes().with_state(Arc::clone(&state));

        post_signal(
            &app,
            serde_json::json!({
                "source": "extension",
                "url": "https://acme.zoom.us/j/1",
                "strong": true
            }),
        )
        .await;

        let db = state.db().await;
        let pending = NotificationRepository::new(&db)
            .pending_on(NotificationChannel::Desktop)
            .expect("pending");

        assert_eq!(pending.len(), 1);
        assert!(pending[0].body.contains("Zoom"), "{}", pending[0].body);
        assert!(
            pending[0].body.contains("Record it?"),
            "{}",
            pending[0].body
        );
    }
}
