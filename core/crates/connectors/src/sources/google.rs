//! Google calendar and mail, through a script the user deploys themselves.
//!
//! # Why not OAuth
//!
//! Calendar read is a *sensitive* scope, needing app verification. Gmail is *restricted*, needing
//! verification plus a paid annual security assessment. And an unverified app in testing mode issues
//! refresh tokens that expire every seven days, so background sync would break weekly.
//!
//! Apps Script runs as the user who deployed it. Authorisation happens once, in Google's own consent
//! screen, for a script the user owns and can read. No Cloud project, no verification, no
//! assessment, no expiry. The script is in `scripts/gapps/`.
//!
//! The cost is honest: a one-time five-step setup that a Microsoft user does not have. That is the
//! price of not paying Google.
//!
//! # Why the window is re-read every time
//!
//! Apps Script exposes no change feed, so there is no cursor to advance through. This re-reads a
//! rolling window and lets `ExternalItemRepository::upsert` make that idempotent — the cursor holds
//! the window's high-water mark rather than a vendor token, which is what
//! `Cursor(Option<String>)` being opaque is for.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::connector::{Connector, SinkConnector, SourceConnector};
use crate::credentials::Secret;
use crate::error::{ConnectorError, Result};
use crate::sources::microsoft::DraftPayload;
use crate::types::{Cursor, ExternalRef, Health, Inbound, Outbound, PullBatch};

/// Credential key holding the deployment URL.
pub const DEPLOYMENT_URL_KEY: &str = "deployment_url";

/// Credential key holding the shared key set in the script.
pub const SHARED_KEY: &str = "shared_key";

/// The script contract this build speaks.
pub const REQUIRED_VERSION: u32 = 1;

/// How far back and forward to read.
///
/// Backwards because meetings get recorded and reconciled after the fact; forwards for knowing what
/// is coming. Ninety days forward covers a quarter of recurring invitations without pulling a
/// calendar's entire future on every poll.
pub const WINDOW_BACK_DAYS: i64 = 30;
pub const WINDOW_FORWARD_DAYS: i64 = 90;

#[derive(Debug)]
pub struct GoogleBridge {
    url: String,
    key: Secret,
    http: reqwest::Client,
}

impl GoogleBridge {
    /// The one place this connector's name is written. See `VaultSink::ID` for why.
    pub const ID: &'static str = "google";

    pub fn new(url: impl Into<String>, key: Secret) -> Self {
        Self {
            url: url.into(),
            key,
            http: reqwest::Client::builder()
                // Apps Script allows six minutes per execution, but a calendar read that takes
                // even thirty seconds is a script that will time out anyway.
                .timeout(Duration::from_secs(45))
                // Apps Script answers a web app request with a 302 to a googleusercontent.com URL
                // that carries the actual body. Without following it, every call returns an empty
                // redirect and the connector looks broken for a reason nothing explains.
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
                .expect("default reqwest client builds"),
        }
    }

    /// Ask the script which contract version it speaks.
    ///
    /// A user's deployment is a version this build does not control, and somebody will still be
    /// running a year-old copy. Checking means the failure names the fix.
    pub async fn version(&self) -> Result<u32> {
        let reply: VersionReply = self
            .call(serde_json::json!({ "action": "version" }))
            .await?;
        Ok(reply.version)
    }

    /// The calendars the account holds, so a user can choose which to import.
    pub async fn calendars(&self) -> Result<Vec<Calendar>> {
        let reply: CalendarsReply = self
            .call(serde_json::json!({ "action": "calendars" }))
            .await?;
        Ok(reply.calendars)
    }

    /// Create a Gmail draft. Never sends — the script has no send call.
    pub async fn create_draft(&self, to: &[String], subject: &str, body: &str) -> Result<DraftRef> {
        let reply: DraftReply = self
            .call(serde_json::json!({
                "action": "createDraft",
                "to": to,
                "subject": subject,
                "body": body,
            }))
            .await?;
        Ok(reply.draft)
    }

    async fn call<T: for<'de> Deserialize<'de>>(&self, mut body: serde_json::Value) -> Result<T> {
        body["key"] = serde_json::json!(self.key.expose());

        let response = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ConnectorError::Transient(format!("could not reach the script: {e}")))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| ConnectorError::Transient(format!("could not read the reply: {e}")))?;

        if !status.is_success() {
            return Err(ConnectorError::Transient(format!(
                "the script answered {status}"
            )));
        }

        // The script reports its own errors in the body with a 200, because a thrown Apps Script
        // error becomes an HTML page and a JSON client reading that gets a decode failure instead
        // of the reason.
        if let Ok(err) = serde_json::from_str::<ErrorReply>(&text) {
            if let Some(message) = err.error {
                return Err(if message == "unauthorised" {
                    ConnectorError::Auth {
                        connector: Self::ID.to_string(),
                    }
                } else {
                    ConnectorError::Permanent(format!("the script refused: {message}"))
                });
            }
        }

        serde_json::from_str(&text).map_err(|e| {
            ConnectorError::Permanent(format!(
                "the script's reply could not be read ({e}). It may be an older deployment — \
                 redeploy it from scripts/gapps/."
            ))
        })
    }
}

#[async_trait]
impl Connector for GoogleBridge {
    fn id(&self) -> &str {
        Self::ID
    }

    fn display_name(&self) -> &str {
        "Google Calendar & Gmail"
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn health(&self) -> Result<Health> {
        match self.version().await {
            Ok(v) if v >= REQUIRED_VERSION => Ok(Health::Ok),
            Ok(v) => Ok(Health::Unavailable(format!(
                "the deployed script speaks version {v}; this build needs {REQUIRED_VERSION}. \
                 Redeploy it from scripts/gapps/."
            ))),
            Err(ConnectorError::Auth { .. }) => Ok(Health::NeedsAuth),
            Err(e) => Ok(Health::Unavailable(e.to_string())),
        }
    }
}

#[async_trait]
impl SourceConnector for GoogleBridge {
    async fn pull(&self, _since: Cursor) -> Result<PullBatch> {
        let now = Utc::now();
        let from = now - chrono::Duration::days(WINDOW_BACK_DAYS);
        let to = now + chrono::Duration::days(WINDOW_FORWARD_DAYS);

        let reply: EventsReply = self
            .call(serde_json::json!({
                "action": "events",
                "from": from.to_rfc3339(),
                "to": to.to_rfc3339(),
            }))
            .await?;

        let items = reply
            .events
            .into_iter()
            .filter_map(|event| match to_inbound(&event) {
                Ok(item) => Some(item),
                Err(e) => {
                    tracing::warn!(id = %event.id, error = %e, "skipping an unreadable event");
                    None
                }
            })
            .collect();

        Ok(PullBatch {
            items,
            // The high-water mark, not a vendor token. Nothing reads it back — the window is
            // recomputed from the clock — but recording it makes the last successful pull visible.
            next_cursor: Cursor(Some(now.to_rfc3339())),
        })
    }
}

/// The script's view of one event.
#[derive(Debug, Clone, Deserialize)]
pub struct ScriptEvent {
    pub id: String,
    #[serde(rename = "calendarId")]
    pub calendar_id: String,
    pub title: Option<String>,
    pub start: String,
    pub end: String,
    #[serde(rename = "isAllDay", default)]
    pub is_all_day: bool,
    pub location: Option<String>,
    pub description: Option<String>,
    pub organizer: Option<String>,
    #[serde(default)]
    pub guests: Vec<ScriptGuest>,
    #[serde(rename = "recurrenceKey")]
    pub recurrence_key: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScriptGuest {
    pub email: String,
    pub name: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Calendar {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub owned: bool,
}

#[async_trait]
impl SinkConnector for GoogleBridge {
    /// Create a draft in the user's own Gmail.
    ///
    /// The deployed script's `createDraft` is `GmailApp.createDraft`, and there is deliberately no
    /// `send` action in the script we ship — so this cannot send mail even if this code asked it to.
    /// That is a stronger guarantee than a comment: the capability is absent from the thing holding
    /// the authorization, which the user can read before deploying it.
    ///
    /// A second push returns the first draft rather than creating another, for the same reason
    /// Microsoft's does — see [`MicrosoftGraph`](crate::sources::MicrosoftGraph)'s `push`.
    async fn push(&self, outbound: &Outbound) -> Result<ExternalRef> {
        if let Some(existing) = &outbound.existing {
            tracing::debug!(
                external_id = %existing.external_id,
                "the draft is already in Gmail; not creating a second"
            );
            return Ok(existing.clone());
        }

        let draft = DraftPayload::from_outbound(&outbound.payload)?;
        let created = self
            .create_draft(&draft.recipients(), &draft.subject, &draft.body)
            .await?;

        Ok(ExternalRef {
            // The draft id rather than the message id: it is what the script's own URL points at,
            // and what a future update would address.
            external_id: created.id,
            url: Some(created.url),
            title: Some(draft.subject),
            remote_version: None,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DraftRef {
    pub id: String,
    #[serde(rename = "messageId")]
    pub message_id: String,
    pub url: String,
}

#[derive(Debug, Deserialize)]
struct VersionReply {
    version: u32,
}

#[derive(Debug, Deserialize)]
struct CalendarsReply {
    calendars: Vec<Calendar>,
}

#[derive(Debug, Deserialize)]
struct EventsReply {
    events: Vec<ScriptEvent>,
}

#[derive(Debug, Deserialize)]
struct DraftReply {
    draft: DraftRef,
}

#[derive(Debug, Deserialize)]
struct ErrorReply {
    error: Option<String>,
}

/// Turn a script event into an inbound item.
///
/// Pure, and the part that can be wrong, so it is tested directly against the shapes the script
/// actually emits.
pub fn to_inbound(event: &ScriptEvent) -> Result<Inbound> {
    let starts = parse_time(&event.start)?;
    let ends = parse_time(&event.end)?;

    // An end before its start is a script bug or a corrupt event; either way the span is unusable
    // for matching a meeting, which is the only thing the event is for.
    if ends < starts {
        return Err(ConnectorError::Permanent(
            "the event ends before it starts".into(),
        ));
    }

    Ok(Inbound {
        external_id: event.id.clone(),
        url: None,
        title: event.title.clone(),
        // No version field from Apps Script, so nothing to compare — upsert overwrites, which is
        // correct: the script's answer is always the current truth.
        remote_version: None,
        occurred_at: Some(starts),
        payload: serde_json::json!({
            "calendar_id": event.calendar_id,
            "starts_at": starts.to_rfc3339(),
            "ends_at": ends.to_rfc3339(),
            "is_all_day": event.is_all_day,
            "location": event.location,
            "join_url": join_url_of(event),
            "organizer_email": event.organizer,
            "recurrence_key": event.recurrence_key,
            "status": normalize_status(event.status.as_deref()),
            "attendees": event.guests.iter().map(|g| serde_json::json!({
                "email": g.email,
                "display_name": g.name,
                "response_status": g.status.as_deref().map(normalize_response),
                "is_organizer": event.organizer.as_deref() == Some(g.email.as_str()),
            })).collect::<Vec<_>>(),
        }),
    })
}

fn parse_time(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| ConnectorError::Permanent(format!("'{raw}' is not a timestamp: {e}")))
}

/// Find a conference link in the event.
///
/// Apps Script exposes no dedicated field for one, so it is pulled out of the location and then the
/// description. First match wins — a description often mentions several URLs and the joining link is
/// conventionally the first.
pub fn join_url_of(event: &ScriptEvent) -> Option<String> {
    const HOSTS: &[&str] = &[
        "meet.google.com",
        "zoom.us",
        "teams.microsoft.com",
        "teams.live.com",
        "whereby.com",
        "meet.jit.si",
    ];

    for haystack in [event.location.as_deref(), event.description.as_deref()]
        .into_iter()
        .flatten()
    {
        for token in haystack.split_whitespace() {
            let token = token.trim_matches(|c: char| {
                !c.is_ascii_alphanumeric()
                    && c != '/'
                    && c != ':'
                    && c != '.'
                    && c != '-'
                    && c != '_'
                    && c != '?'
                    && c != '='
                    && c != '&'
            });
            if !token.starts_with("http") {
                continue;
            }
            if HOSTS.iter().any(|host| token.contains(host)) {
                return Some(token.to_string());
            }
        }
    }
    None
}

/// Map Apps Script's guest status onto the vocabulary the schema stores.
fn normalize_response(raw: &str) -> &'static str {
    match raw.trim().to_ascii_uppercase().as_str() {
        "YES" | "OWNER" | "ACCEPTED" => "accepted",
        "NO" | "DECLINED" => "declined",
        "MAYBE" | "TENTATIVE" => "tentative",
        _ => "none",
    }
}

/// Map the event's own status. Anything unrecognised is confirmed, because an event the user can
/// see in their calendar is happening as far as they are concerned.
fn normalize_status(raw: Option<&str>) -> &'static str {
    match raw.map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("NO") | Some("DECLINED") | Some("CANCELLED") => "cancelled",
        Some("MAYBE") | Some("TENTATIVE") | Some("INVITED") => "tentative",
        _ => "confirmed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> ScriptEvent {
        ScriptEvent {
            id: "evt-1".into(),
            calendar_id: "primary".into(),
            title: Some("Platform standup".into()),
            start: "2026-08-20T09:00:00Z".into(),
            end: "2026-08-20T09:30:00Z".into(),
            is_all_day: false,
            location: None,
            description: None,
            organizer: Some("me@example.com".into()),
            guests: vec![],
            recurrence_key: None,
            status: Some("YES".into()),
        }
    }

    #[test]
    fn the_id_is_a_stored_value_and_cannot_drift() {
        assert_eq!(GoogleBridge::ID, "google");
    }

    #[test]
    fn an_event_becomes_an_inbound_item() {
        let item = to_inbound(&event()).expect("decodes");

        assert_eq!(item.external_id, "evt-1");
        assert_eq!(item.title.as_deref(), Some("Platform standup"));
        assert_eq!(item.payload["starts_at"], "2026-08-20T09:00:00+00:00");
        assert_eq!(item.payload["status"], "confirmed");
    }

    #[test]
    fn a_bad_timestamp_is_reported_not_panicked() {
        let mut bad = event();
        bad.start = "not a time".into();
        assert!(to_inbound(&bad).is_err());
    }

    /// The span is the only thing the event is for, so an impossible one is refused rather than
    /// stored and matched against.
    #[test]
    fn an_event_ending_before_it_starts_is_refused() {
        let mut backwards = event();
        backwards.end = "2026-08-20T08:00:00Z".into();
        assert!(to_inbound(&backwards).is_err());
    }

    #[test]
    fn guest_statuses_are_mapped_to_the_stored_vocabulary() {
        let mut e = event();
        e.guests = vec![
            ScriptGuest {
                email: "me@example.com".into(),
                name: Some("Me".into()),
                status: Some("OWNER".into()),
            },
            ScriptGuest {
                email: "priya@example.com".into(),
                name: Some("Priya".into()),
                status: Some("MAYBE".into()),
            },
            ScriptGuest {
                email: "sam@example.com".into(),
                name: None,
                status: Some("NO".into()),
            },
        ];

        let item = to_inbound(&e).expect("decodes");
        let attendees = item.payload["attendees"].as_array().expect("attendees");

        assert_eq!(attendees[0]["response_status"], "accepted");
        assert_eq!(attendees[0]["is_organizer"], true);
        assert_eq!(attendees[1]["response_status"], "tentative");
        assert_eq!(attendees[2]["response_status"], "declined");
        assert_eq!(attendees[2]["is_organizer"], false);
    }

    #[test]
    fn an_unknown_guest_status_is_none_rather_than_a_guess() {
        assert_eq!(normalize_response("SOMETHING"), "none");
        assert_eq!(normalize_response(""), "none");
    }

    /// An event the user can see in their calendar is happening, whatever unfamiliar string the
    /// script reported.
    #[test]
    fn an_unknown_event_status_is_confirmed() {
        assert_eq!(normalize_status(None), "confirmed");
        assert_eq!(normalize_status(Some("OWNER")), "confirmed");
        assert_eq!(normalize_status(Some("wat")), "confirmed");
        assert_eq!(normalize_status(Some("declined")), "cancelled");
        assert_eq!(normalize_status(Some("INVITED")), "tentative");
    }

    #[test]
    fn a_join_link_is_found_in_the_location() {
        let mut e = event();
        e.location = Some("https://meet.google.com/abc-defg-hij".into());
        assert_eq!(
            join_url_of(&e).as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
    }

    #[test]
    fn a_join_link_is_found_in_the_description() {
        let mut e = event();
        e.description = Some("Join here: https://zoom.us/j/123456789 (dial-in below)".into());
        assert_eq!(
            join_url_of(&e).as_deref(),
            Some("https://zoom.us/j/123456789")
        );
    }

    #[test]
    fn the_location_wins_over_the_description() {
        let mut e = event();
        e.location = Some("https://meet.google.com/aaa".into());
        e.description = Some("https://zoom.us/j/999".into());
        assert_eq!(
            join_url_of(&e).as_deref(),
            Some("https://meet.google.com/aaa")
        );
    }

    #[test]
    fn an_unrelated_url_is_not_a_join_link() {
        let mut e = event();
        e.description = Some("Agenda at https://example.com/docs/agenda".into());
        assert_eq!(join_url_of(&e), None);
    }

    #[test]
    fn no_link_anywhere_is_none() {
        assert_eq!(join_url_of(&event()), None);
    }

    #[test]
    fn an_all_day_event_keeps_its_flag() {
        let mut e = event();
        e.is_all_day = true;
        let item = to_inbound(&e).expect("decodes");
        assert_eq!(item.payload["is_all_day"], true);
    }

    #[test]
    fn a_recurring_instance_carries_its_series_key() {
        let mut e = event();
        e.recurrence_key = Some("series-abc".into());
        let item = to_inbound(&e).expect("decodes");
        assert_eq!(item.payload["recurrence_key"], "series-abc");
    }

    /// A stand-in for a deployed script, so the request path — auth, error shape, redirect — is
    /// exercised rather than only the decoding.
    async fn stub(reply: serde_json::Value, redirect: bool) -> String {
        use axum::{routing::post, Router};
        use std::sync::Arc;

        let payload = Arc::new(reply);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        let served = Arc::clone(&payload);
        let app = Router::new()
            .route(
                "/exec",
                post(move |body: String| {
                    let payload = Arc::clone(&served);
                    async move {
                        // The key is checked, because a deployment reachable by URL alone is what
                        // the shared key exists to protect against.
                        let parsed: serde_json::Value =
                            serde_json::from_str(&body).unwrap_or_default();
                        if parsed["key"] != "secret" {
                            return axum::Json(serde_json::json!({ "error": "unauthorised" }));
                        }
                        axum::Json((*payload).clone())
                    }
                }),
            )
            // Apps Script answers a web app POST with a redirect to googleusercontent.com carrying
            // the real body. A client that does not follow it sees nothing.
            .route(
                "/redirecting",
                post(move || async move { axum::response::Redirect::temporary("/exec") }),
            );

        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let path = if redirect { "redirecting" } else { "exec" };
        format!("http://{addr}/{path}")
    }

    fn bridge(url: String) -> GoogleBridge {
        GoogleBridge::new(url, Secret::new("secret"))
    }

    #[tokio::test]
    async fn a_pull_reaches_the_script_and_returns_its_events() {
        let url = stub(
            serde_json::json!({
                "version": 1,
                "events": [{
                    "id": "evt-1",
                    "calendarId": "primary",
                    "title": "Standup",
                    "start": "2026-08-20T09:00:00.000Z",
                    "end": "2026-08-20T09:30:00.000Z",
                    "isAllDay": false,
                    "organizer": "me@example.com",
                    "guests": [],
                    "status": "OWNER"
                }]
            }),
            false,
        )
        .await;

        let batch = bridge(url).pull(Cursor::start()).await.expect("pull");
        assert_eq!(batch.items.len(), 1);
        assert_eq!(batch.items[0].external_id, "evt-1");
        assert!(
            batch.next_cursor.0.is_some(),
            "the high-water mark makes the last successful pull visible"
        );
    }

    /// Without following the redirect every call returns an empty body and the connector looks
    /// broken for a reason nothing explains.
    #[tokio::test]
    async fn the_apps_script_redirect_is_followed() {
        let url = stub(serde_json::json!({ "version": 1, "events": [] }), true).await;

        let batch = bridge(url).pull(Cursor::start()).await.expect("pull");
        assert!(batch.items.is_empty());
    }

    #[tokio::test]
    async fn a_wrong_shared_key_is_an_auth_error() {
        let url = stub(serde_json::json!({ "version": 1, "events": [] }), false).await;
        let wrong = GoogleBridge::new(url, Secret::new("not-the-key"));

        let err = wrong.pull(Cursor::start()).await.expect_err("must refuse");
        assert!(
            matches!(err, ConnectorError::Auth { .. }),
            "an unauthorised reply has to become NeedsReauth, not a generic failure: {err:?}"
        );
    }

    #[tokio::test]
    async fn an_old_deployment_is_reported_as_needing_a_redeploy() {
        let url = stub(serde_json::json!({ "version": 0 }), false).await;
        let health = bridge(url).health().await.expect("health");

        match health {
            Health::Unavailable(reason) => assert!(reason.contains("Redeploy"), "{reason}"),
            other => panic!("expected a version complaint, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_current_deployment_is_healthy() {
        let url = stub(serde_json::json!({ "version": 1 }), false).await;
        assert_eq!(bridge(url).health().await.expect("health"), Health::Ok);
    }

    #[tokio::test]
    async fn an_unreachable_script_is_transient_not_permanent() {
        // Port 1 is reserved and nothing listens there.
        let bridge = GoogleBridge::new("http://127.0.0.1:1/exec", Secret::new("secret"));
        let err = bridge.pull(Cursor::start()).await.expect_err("must fail");

        assert!(
            matches!(err, ConnectorError::Transient(_)),
            "a script that is briefly unreachable must be retried, not given up on: {err:?}"
        );
    }

    #[tokio::test]
    async fn a_draft_is_created_and_its_link_returned() {
        let url = stub(
            serde_json::json!({
                "version": 1,
                "draft": {
                    "id": "d-1",
                    "messageId": "m-1",
                    "url": "https://mail.google.com/mail/u/0/#drafts?compose=m-1"
                }
            }),
            false,
        )
        .await;

        let draft = bridge(url)
            .create_draft(&["a@b.com".into()], "Follow-up", "body")
            .await
            .expect("draft");

        assert_eq!(draft.message_id, "m-1");
        assert!(draft.url.contains("compose=m-1"));
    }

    #[tokio::test]
    async fn an_unreadable_event_is_skipped_rather_than_failing_the_batch() {
        let url = stub(
            serde_json::json!({
                "version": 1,
                "events": [
                    { "id": "bad", "calendarId": "p", "start": "nope", "end": "nope", "guests": [] },
                    {
                        "id": "good",
                        "calendarId": "p",
                        "start": "2026-08-20T09:00:00.000Z",
                        "end": "2026-08-20T09:30:00.000Z",
                        "guests": []
                    }
                ]
            }),
            false,
        )
        .await;

        let batch = bridge(url).pull(Cursor::start()).await.expect("pull");
        assert_eq!(batch.items.len(), 1, "one bad event must not lose the rest");
        assert_eq!(batch.items[0].external_id, "good");
    }

    #[test]
    fn the_script_reply_shape_deserializes() {
        let raw = r#"{
            "version": 1,
            "events": [{
                "id": "evt-1",
                "calendarId": "primary",
                "title": "Standup",
                "start": "2026-08-20T09:00:00.000Z",
                "end": "2026-08-20T09:30:00.000Z",
                "isAllDay": false,
                "location": null,
                "description": null,
                "organizer": "me@example.com",
                "guests": [{"email": "a@b.com", "name": "A", "status": "YES"}],
                "recurrenceKey": null,
                "status": "OWNER"
            }]
        }"#;

        let reply: EventsReply = serde_json::from_str(raw).expect("deserializes");
        assert_eq!(reply.events.len(), 1);
        assert!(to_inbound(&reply.events[0]).is_ok());
    }
    /// The sink, through the same stub the pull path uses. Proves the payload shape reaches the
    /// script and the link comes back — which is what the interface turns into "Open in Gmail".
    #[tokio::test]
    async fn the_sink_creates_a_draft_and_returns_its_link() {
        let url = stub(
            serde_json::json!({
                "version": REQUIRED_VERSION,
                "draft": { "id": "d-1", "messageId": "m-1", "url": "https://mail.google.com/d/1" }
            }),
            false,
        )
        .await;

        let bridge = GoogleBridge::new(url, Secret::new("secret"));
        let reference = bridge
            .push(&draft_outbound(serde_json::json!({
                "to": ["priya@example.com"],
                "subject": "Follow-up: Platform standup",
                "body": "Here is what we agreed."
            })))
            .await
            .expect("a draft");

        assert_eq!(reference.external_id, "d-1");
        assert_eq!(
            reference.url.as_deref(),
            Some("https://mail.google.com/d/1")
        );
        assert_eq!(
            reference.title.as_deref(),
            Some("Follow-up: Platform standup")
        );
    }

    /// Two identical follow-ups in somebody's drafts folder is precisely the duplicate the outbox
    /// exists to prevent.
    #[tokio::test]
    async fn a_second_push_returns_the_first_draft_without_creating_another() {
        // A stub that would answer, so a second creation would visibly succeed if it happened.
        let url = stub(
            serde_json::json!({
                "version": REQUIRED_VERSION,
                "draft": { "id": "d-2", "messageId": "m-2", "url": "https://mail.google.com/d/2" }
            }),
            false,
        )
        .await;

        let bridge = GoogleBridge::new(url, Secret::new("secret"));
        let mut outbound = draft_outbound(serde_json::json!({
            "to": ["priya@example.com"],
            "subject": "Follow-up",
            "body": "body"
        }));
        outbound.existing = Some(ExternalRef {
            external_id: "d-1".into(),
            url: Some("https://mail.google.com/d/1".into()),
            title: Some("Follow-up".into()),
            remote_version: None,
        });

        let reference = bridge.push(&outbound).await.expect("the existing draft");
        assert_eq!(
            reference.external_id, "d-1",
            "the first draft, not a second"
        );
    }

    /// A draft the user cannot send is not worth creating, and a 400 three layers down says nothing
    /// about why.
    #[tokio::test]
    async fn a_draft_with_no_recipient_is_refused_before_the_script_is_called() {
        let bridge = GoogleBridge::new("http://127.0.0.1:1/exec", Secret::new("k"));

        let refused = bridge
            .push(&draft_outbound(serde_json::json!({
                "to": [],
                "subject": "Follow-up",
                "body": "body"
            })))
            .await;

        // The URL points at nothing, so reaching the network would be a transient error. A
        // permanent one proves the refusal happened first.
        assert!(
            matches!(refused, Err(ConnectorError::Permanent(ref m)) if m.contains("recipient")),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn a_draft_with_no_subject_is_refused() {
        let bridge = GoogleBridge::new("http://127.0.0.1:1/exec", Secret::new("k"));

        let refused = bridge
            .push(&draft_outbound(serde_json::json!({
                "to": ["a@b.com"],
                "subject": "   ",
                "body": "body"
            })))
            .await;
        assert!(
            matches!(refused, Err(ConnectorError::Permanent(ref m)) if m.contains("subject")),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn something_that_is_not_a_draft_is_refused_rather_than_sent() {
        let bridge = GoogleBridge::new("http://127.0.0.1:1/exec", Secret::new("k"));

        let refused = bridge
            .push(&draft_outbound(
                serde_json::json!({ "markdown": "# a meeting" }),
            ))
            .await;
        assert!(
            matches!(refused, Err(ConnectorError::Permanent(_))),
            "{refused:?}"
        );
    }

    /// The one guarantee worth asserting structurally: this connector asks the script for four
    /// things, and none of them sends mail.
    ///
    /// Held as an allowlist rather than a denylist. A denylist can only forbid the send action
    /// somebody thought of; this fails for any action added without being named here, which is the
    /// same reasoning `MUTATING_TOOLS` uses in `mcp-server`.
    #[test]
    fn the_connector_asks_the_script_for_nothing_but_these() {
        let source = include_str!("google.rs");
        // Assembled so this test does not match itself.
        let marker = concat!("\"action\"", ": \"");

        let mut asked: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for (index, _) in source.match_indices(marker) {
            let rest = &source[index + marker.len()..];
            if let Some(end) = rest.find('"') {
                asked.insert(&rest[..end]);
            }
        }

        let permitted: std::collections::BTreeSet<&str> =
            ["version", "calendars", "events", "createDraft"]
                .into_iter()
                .collect();

        assert!(
            asked.is_subset(&permitted),
            "the connector asks the script for something unlisted: {:?}",
            asked.difference(&permitted).collect::<Vec<_>>()
        );
        assert!(
            asked.contains("createDraft"),
            "the draft action should be in use; found {asked:?}"
        );
    }

    fn draft_outbound(payload: serde_json::Value) -> Outbound {
        Outbound {
            node_kind: "email_draft".into(),
            node_id: notewise_storage::Id::new(),
            operation: crate::types::Operation::Create,
            payload,
            existing: None,
        }
    }
}
