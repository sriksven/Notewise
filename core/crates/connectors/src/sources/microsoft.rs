//! Outlook calendar and mail, through Microsoft Graph.
//!
//! # Why this one is a button and Google is a five-step setup
//!
//! Graph is free, `Calendars.Read` and `Mail.ReadWrite` are delegated scopes a user consents to
//! themselves without tenant-admin approval, and publisher verification is optional. So Microsoft
//! gets what Google cannot: one click. The asymmetry is not a design preference, it is the shape of
//! the two vendors' policies.
//!
//! The client id is embedded in the binary, which is correct for a public PKCE client — there is no
//! secret to leak. A bring-your-own client id remains configurable for tenants whose policy requires
//! their own app registration.
//!
//! # Why the cursor is a whole URL
//!
//! Graph's delta query hands back an opaque `@odata.deltaLink` to call next time. Unlike the Google
//! bridge, which re-reads a window because Apps Script has no change feed, this is a real
//! incremental cursor. `Cursor(Option<String>)` being opaque is what lets the two mean entirely
//! different things without a shared schema.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::connector::{Connector, SourceConnector};
use crate::credentials::Secret;
use crate::error::{ConnectorError, Result};
use crate::types::{Cursor, Health, Inbound, PullBatch};

/// Credential key holding the refresh token.
pub const REFRESH_TOKEN_KEY: &str = "refresh_token";

/// Credential key holding a bring-your-own client id, when a tenant requires one.
pub const CLIENT_ID_KEY: &str = "client_id";

/// The scopes asked for.
///
/// `offline_access` is what yields a refresh token; without it the grant lasts an hour and
/// background sync stops working the moment the user closes the app.
///
/// Deliberately no `Mail.Send`. There is no send path in this product and asking for the scope would
/// be requesting permission for something nothing does.
pub const SCOPES: &str = "offline_access Calendars.Read Mail.ReadWrite";

const GRAPH: &str = "https://graph.microsoft.com/v1.0";
const TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";
const AUTHORIZE_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/authorize";

/// URL-safe base64 without padding, as PKCE requires.
///
/// Written out rather than taking a dependency: it is twenty lines, and the alternative is a crate
/// in the tree for one encoding used in one place.
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    let mut out = String::with_capacity((bytes.len() * 4).div_ceil(3));
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);

        // One output character per six input bits, dropping the characters that would only encode
        // padding — which is what "without padding" means and why the length depends on the
        // remainder.
        let take = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for i in 0..take {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3F) as usize] as char);
        }
    }
    out
}

/// A PKCE verifier and the challenge derived from it.
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// Generate a fresh pair.
    ///
    /// Two v4 UUIDs in hex: 64 characters of unreserved alphabet, inside PKCE's 43-128 range, with
    /// no RNG dependency taken for one call site — the same reasoning the webhook signing secret
    /// uses.
    pub fn generate() -> Self {
        let verifier = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        Self::from_verifier(verifier)
    }

    pub fn from_verifier(verifier: String) -> Self {
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

/// The URL to open in the user's browser.
pub fn authorize_url(client_id: &str, redirect_uri: &str, pkce: &Pkce, state: &str) -> String {
    let encode = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                other => format!("%{other:02X}"),
            })
            .collect::<String>()
    };

    format!(
        "{AUTHORIZE_URL}?client_id={}&response_type=code&redirect_uri={}&response_mode=query\
         &scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        encode(client_id),
        encode(redirect_uri),
        encode(SCOPES),
        encode(&pkce.challenge),
        encode(state),
    )
}

#[derive(Debug)]
pub struct MicrosoftGraph {
    client_id: String,
    refresh_token: Secret,
    /// Overridable so tests can point at a stub instead of Microsoft.
    graph_base: String,
    token_url: String,
    http: reqwest::Client,
}

impl MicrosoftGraph {
    /// The one place this connector's name is written.
    pub const ID: &'static str = "microsoft";

    pub fn new(client_id: impl Into<String>, refresh_token: Secret) -> Self {
        Self {
            client_id: client_id.into(),
            refresh_token,
            graph_base: GRAPH.to_string(),
            token_url: TOKEN_URL.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("default reqwest client builds"),
        }
    }

    /// Point at a different Graph, for tests.
    pub fn with_endpoints(
        mut self,
        graph_base: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        self.graph_base = graph_base.into();
        self.token_url = token_url.into();
        self
    }

    /// Exchange the refresh token for an access token.
    ///
    /// Done per pull rather than cached: an access token lasts an hour, a pull happens every few
    /// minutes, and caching one would mean holding a bearer token in memory to save a request that
    /// is cheap. A refresh that fails with `invalid_grant` is the user having revoked access, which
    /// is `Auth` and not something to retry.
    async fn access_token(&self) -> Result<String> {
        let params = [
            ("client_id", self.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", self.refresh_token.expose()),
            ("scope", SCOPES),
        ];

        let response = self
            .http
            .post(&self.token_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| ConnectorError::Transient(format!("could not reach Microsoft: {e}")))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| ConnectorError::Transient(format!("could not read the reply: {e}")))?;

        if !status.is_success() {
            // A revoked or expired grant is not a transient problem — retrying it forever would
            // hide the fact that the user has to sign in again.
            return Err(
                if text.contains("invalid_grant") || status.as_u16() == 400 {
                    ConnectorError::Auth {
                        connector: Self::ID.to_string(),
                    }
                } else {
                    ConnectorError::Transient(format!("Microsoft answered {status}"))
                },
            );
        }

        let token: TokenReply = serde_json::from_str(&text)
            .map_err(|e| ConnectorError::Permanent(format!("unreadable token reply: {e}")))?;
        Ok(token.access_token)
    }
}

#[async_trait]
impl Connector for MicrosoftGraph {
    fn id(&self) -> &str {
        Self::ID
    }

    fn display_name(&self) -> &str {
        "Outlook Calendar & Mail"
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn health(&self) -> Result<Health> {
        match self.access_token().await {
            Ok(_) => Ok(Health::Ok),
            Err(ConnectorError::Auth { .. }) => Ok(Health::NeedsAuth),
            Err(e) => Ok(Health::Unavailable(e.to_string())),
        }
    }
}

#[async_trait]
impl SourceConnector for MicrosoftGraph {
    async fn pull(&self, since: Cursor) -> Result<PullBatch> {
        let token = self.access_token().await?;

        // A stored delta link is followed verbatim; a first run asks for a window. Graph refuses a
        // delta request that also carries its own parameters, so these are exclusive.
        let url = match since.as_deref() {
            Some(link) if link.starts_with("http") => link.to_string(),
            _ => {
                let now = Utc::now();
                format!(
                    "{}/me/calendarView/delta?startDateTime={}&endDateTime={}",
                    self.graph_base,
                    (now - chrono::Duration::days(30)).to_rfc3339(),
                    (now + chrono::Duration::days(90)).to_rfc3339(),
                )
            }
        };

        let response = self
            .http
            .get(&url)
            .bearer_auth(&token)
            // Without this, `dateTime` comes back in the mailbox's own zone with no offset on it,
            // and every timestamp is silently wrong by however far that is from UTC.
            .header("Prefer", "outlook.timezone=\"UTC\"")
            .send()
            .await
            .map_err(|e| ConnectorError::Transient(format!("could not reach Graph: {e}")))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| ConnectorError::Transient(format!("could not read the reply: {e}")))?;

        // An expired delta token is recovery, not failure: drop the cursor and re-read the window.
        // Upsert makes the re-read harmless.
        if status.as_u16() == 410 {
            tracing::info!("the delta token expired; restarting from a fresh window");
            return Ok(PullBatch {
                items: Vec::new(),
                next_cursor: Cursor::start(),
            });
        }
        if status.as_u16() == 401 {
            return Err(ConnectorError::Auth {
                connector: Self::ID.to_string(),
            });
        }
        if !status.is_success() {
            return Err(ConnectorError::Transient(format!(
                "Graph answered {status}"
            )));
        }

        let reply: DeltaReply = serde_json::from_str(&text)
            .map_err(|e| ConnectorError::Permanent(format!("unreadable Graph reply: {e}")))?;

        let items = reply
            .value
            .iter()
            .filter_map(|event| match to_inbound(event) {
                Ok(item) => item,
                Err(e) => {
                    tracing::warn!(id = %event.id, error = %e, "skipping an unreadable event");
                    None
                }
            })
            .collect();

        // The delta link if the page is the last one, otherwise the next link so the following pull
        // continues the same page walk rather than starting over.
        Ok(PullBatch {
            items,
            next_cursor: Cursor(reply.delta_link.or(reply.next_link).or(since.0)),
        })
    }
}

#[derive(Debug, Deserialize)]
struct TokenReply {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct DeltaReply {
    #[serde(default)]
    value: Vec<GraphEvent>,
    #[serde(rename = "@odata.deltaLink")]
    delta_link: Option<String>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GraphEvent {
    pub id: String,
    pub subject: Option<String>,
    pub start: Option<GraphTime>,
    pub end: Option<GraphTime>,
    #[serde(rename = "isAllDay", default)]
    pub is_all_day: bool,
    #[serde(rename = "isCancelled", default)]
    pub is_cancelled: bool,
    pub location: Option<GraphLocation>,
    #[serde(rename = "onlineMeeting")]
    pub online_meeting: Option<GraphOnlineMeeting>,
    pub organizer: Option<GraphAttendee>,
    #[serde(default)]
    pub attendees: Vec<GraphAttendee>,
    #[serde(rename = "seriesMasterId")]
    pub series_master_id: Option<String>,
    /// Present when the delta feed is reporting a deletion.
    #[serde(rename = "@removed")]
    pub removed: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GraphTime {
    #[serde(rename = "dateTime")]
    pub date_time: String,
    #[serde(rename = "timeZone")]
    pub time_zone: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GraphLocation {
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GraphOnlineMeeting {
    #[serde(rename = "joinUrl")]
    pub join_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GraphAttendee {
    #[serde(rename = "emailAddress")]
    pub email_address: Option<GraphEmail>,
    pub status: Option<GraphResponse>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GraphEmail {
    pub address: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GraphResponse {
    pub response: Option<String>,
}

/// Turn a Graph event into an inbound item.
///
/// `Ok(None)` for a deletion, which is a thing the delta feed reports and not an error.
pub fn to_inbound(event: &GraphEvent) -> Result<Option<Inbound>> {
    if event.removed.is_some() {
        return Ok(None);
    }

    let (Some(start), Some(end)) = (event.start.as_ref(), event.end.as_ref()) else {
        return Err(ConnectorError::Permanent("the event has no span".into()));
    };

    let starts = parse_graph_time(start)?;
    let ends = parse_graph_time(end)?;
    if ends < starts {
        return Err(ConnectorError::Permanent(
            "the event ends before it starts".into(),
        ));
    }

    let organizer_email = event
        .organizer
        .as_ref()
        .and_then(|o| o.email_address.as_ref())
        .and_then(|e| e.address.clone());

    let attendees: Vec<_> = event
        .attendees
        .iter()
        .filter_map(|a| {
            let email = a.email_address.as_ref()?.address.clone()?;
            Some(serde_json::json!({
                "email": email,
                "display_name": a.email_address.as_ref().and_then(|e| e.name.clone()),
                "response_status": normalize_response(
                    a.status.as_ref().and_then(|s| s.response.as_deref()),
                ),
                "is_organizer": organizer_email.as_deref() == a
                    .email_address
                    .as_ref()
                    .and_then(|e| e.address.as_deref()),
            }))
        })
        .collect();

    Ok(Some(Inbound {
        external_id: event.id.clone(),
        url: None,
        title: event.subject.clone(),
        remote_version: None,
        occurred_at: Some(starts),
        payload: serde_json::json!({
            "calendar_id": "primary",
            "starts_at": starts.to_rfc3339(),
            "ends_at": ends.to_rfc3339(),
            "is_all_day": event.is_all_day,
            "location": event.location.as_ref().and_then(|l| l.display_name.clone()),
            "join_url": event.online_meeting.as_ref().and_then(|m| m.join_url.clone()),
            "organizer_email": organizer_email,
            "recurrence_key": event.series_master_id,
            "status": if event.is_cancelled { "cancelled" } else { "confirmed" },
            "attendees": attendees,
        }),
    }))
}

/// Parse Graph's split date-and-zone.
///
/// Graph sends `dateTime` with no offset and the zone in a separate field. With the
/// `Prefer: outlook.timezone="UTC"` header it is UTC, which is what the request asks for — but the
/// value still parses as naive, so it has to be attached to a zone here rather than assumed to
/// carry one. Getting this wrong shifts every meeting by the mailbox's offset.
pub fn parse_graph_time(time: &GraphTime) -> Result<DateTime<Utc>> {
    let raw = time.date_time.trim();

    // Some responses do carry an offset. Honour it when present rather than double-applying a zone.
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        return Ok(parsed.with_timezone(&Utc));
    }

    let zone = time.time_zone.as_deref().unwrap_or("UTC");
    if !zone.eq_ignore_ascii_case("utc") {
        return Err(ConnectorError::Permanent(format!(
            "the event is in '{zone}' rather than UTC; the request asks for UTC, so this reply is \
             unexpected and guessing an offset would move the meeting"
        )));
    }

    // Graph emits seven fractional digits, which is more than `%.f` accepts in some builds, so the
    // fraction is trimmed rather than parsed.
    let trimmed = raw.split('.').next().unwrap_or(raw);
    NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S")
        .map(|naive| naive.and_utc())
        .map_err(|e| ConnectorError::Permanent(format!("'{raw}' is not a timestamp: {e}")))
}

fn normalize_response(raw: Option<&str>) -> &'static str {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("accepted") | Some("organizer") => "accepted",
        Some("declined") => "declined",
        Some("tentativelyaccepted") => "tentative",
        _ => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(raw: &str) -> GraphTime {
        GraphTime {
            date_time: raw.into(),
            time_zone: Some("UTC".into()),
        }
    }

    fn event() -> GraphEvent {
        GraphEvent {
            id: "AAMk-1".into(),
            subject: Some("Platform standup".into()),
            start: Some(time("2026-08-20T09:00:00.0000000")),
            end: Some(time("2026-08-20T09:30:00.0000000")),
            is_all_day: false,
            is_cancelled: false,
            location: None,
            online_meeting: None,
            organizer: None,
            attendees: vec![],
            series_master_id: None,
            removed: None,
        }
    }

    #[test]
    fn the_id_is_a_stored_value_and_cannot_drift() {
        assert_eq!(MicrosoftGraph::ID, "microsoft");
    }

    /// PKCE's own spec vectors, so the encoder is checked against something external rather than
    /// against itself.
    #[test]
    fn base64url_matches_the_pkce_example() {
        // RFC 7636 appendix B: this verifier produces this challenge.
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn base64url_has_no_padding_and_no_unsafe_characters() {
        for len in 1..40 {
            let encoded = base64url(&vec![0xFFu8; len]);
            assert!(!encoded.contains('='), "{encoded}");
            assert!(!encoded.contains('+'), "{encoded}");
            assert!(!encoded.contains('/'), "{encoded}");
        }
    }

    #[test]
    fn base64url_encodes_the_classic_examples() {
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(b""), "");
    }

    #[test]
    fn a_generated_verifier_is_within_the_permitted_length() {
        let pkce = Pkce::generate();
        assert!(
            (43..=128).contains(&pkce.verifier.len()),
            "{}",
            pkce.verifier.len()
        );
        assert_ne!(Pkce::generate().verifier, pkce.verifier);
    }

    #[test]
    fn the_authorize_url_carries_the_challenge_and_not_the_verifier() {
        let pkce = Pkce::from_verifier("verifier-value-that-is-long-enough-to-be-valid".into());
        let url = authorize_url("client-1", "http://127.0.0.1:4321/callback", &pkce, "st");

        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&pkce.challenge));
        assert!(
            !url.contains("verifier-value"),
            "the verifier must never leave this process"
        );
        assert!(url.contains("127.0.0.1"), "{url}");
    }

    /// No send scope is requested, because nothing in this product sends.
    #[test]
    fn the_scopes_ask_for_no_send_permission() {
        assert!(!SCOPES.to_lowercase().contains("mail.send"));
        assert!(SCOPES.contains("offline_access"));
        assert!(SCOPES.contains("Calendars.Read"));
    }

    #[test]
    fn an_event_becomes_an_inbound_item() {
        let item = to_inbound(&event()).expect("decodes").expect("not removed");
        assert_eq!(item.external_id, "AAMk-1");
        assert_eq!(item.payload["starts_at"], "2026-08-20T09:00:00+00:00");
        assert_eq!(item.payload["status"], "confirmed");
    }

    /// The subtlety Graph has and Google does not: seven fractional digits, no offset.
    #[test]
    fn graphs_fractional_seconds_are_handled() {
        let parsed = parse_graph_time(&time("2026-08-20T09:00:00.0000000")).expect("parses");
        assert_eq!(parsed.to_rfc3339(), "2026-08-20T09:00:00+00:00");
    }

    #[test]
    fn a_timestamp_that_does_carry_an_offset_is_honoured_rather_than_double_zoned() {
        let parsed = parse_graph_time(&GraphTime {
            date_time: "2026-08-20T09:00:00+02:00".into(),
            time_zone: Some("UTC".into()),
        })
        .expect("parses");
        assert_eq!(parsed.to_rfc3339(), "2026-08-20T07:00:00+00:00");
    }

    /// Guessing an offset would move the meeting, so an unexpected zone is refused instead.
    #[test]
    fn a_non_utc_zone_is_refused_rather_than_guessed() {
        let err = parse_graph_time(&GraphTime {
            date_time: "2026-08-20T09:00:00.0000000".into(),
            time_zone: Some("Pacific Standard Time".into()),
        })
        .expect_err("must refuse");
        assert!(err.to_string().contains("Pacific"), "{err}");
    }

    #[test]
    fn a_removed_event_is_not_an_error() {
        let mut removed = event();
        removed.removed = Some(serde_json::json!({ "reason": "deleted" }));
        assert!(to_inbound(&removed).expect("decodes").is_none());
    }

    #[test]
    fn an_event_with_no_span_is_refused() {
        let mut spanless = event();
        spanless.start = None;
        assert!(to_inbound(&spanless).is_err());
    }

    #[test]
    fn a_cancelled_event_is_marked_as_such() {
        let mut cancelled = event();
        cancelled.is_cancelled = true;
        let item = to_inbound(&cancelled).expect("decodes").expect("present");
        assert_eq!(item.payload["status"], "cancelled");
    }

    #[test]
    fn attendees_and_the_organizer_are_mapped() {
        let mut e = event();
        e.organizer = Some(GraphAttendee {
            email_address: Some(GraphEmail {
                address: Some("me@example.com".into()),
                name: Some("Me".into()),
            }),
            status: None,
        });
        e.attendees = vec![
            GraphAttendee {
                email_address: Some(GraphEmail {
                    address: Some("me@example.com".into()),
                    name: Some("Me".into()),
                }),
                status: Some(GraphResponse {
                    response: Some("organizer".into()),
                }),
            },
            GraphAttendee {
                email_address: Some(GraphEmail {
                    address: Some("priya@example.com".into()),
                    name: Some("Priya".into()),
                }),
                status: Some(GraphResponse {
                    response: Some("tentativelyAccepted".into()),
                }),
            },
        ];

        let item = to_inbound(&e).expect("decodes").expect("present");
        let attendees = item.payload["attendees"].as_array().expect("attendees");

        assert_eq!(attendees[0]["response_status"], "accepted");
        assert_eq!(attendees[0]["is_organizer"], true);
        assert_eq!(attendees[1]["response_status"], "tentative");
        assert_eq!(attendees[1]["is_organizer"], false);
    }

    #[test]
    fn an_attendee_without_an_address_is_dropped_rather_than_stored_blank() {
        let mut e = event();
        e.attendees = vec![GraphAttendee {
            email_address: None,
            status: None,
        }];
        let item = to_inbound(&e).expect("decodes").expect("present");
        assert!(item.payload["attendees"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_join_url_comes_from_the_online_meeting_field() {
        let mut e = event();
        e.online_meeting = Some(GraphOnlineMeeting {
            join_url: Some("https://teams.microsoft.com/l/meetup-join/x".into()),
        });
        let item = to_inbound(&e).expect("decodes").expect("present");
        assert_eq!(
            item.payload["join_url"],
            "https://teams.microsoft.com/l/meetup-join/x"
        );
    }

    #[test]
    fn a_recurring_instance_carries_its_series_master() {
        let mut e = event();
        e.series_master_id = Some("master-1".into());
        let item = to_inbound(&e).expect("decodes").expect("present");
        assert_eq!(item.payload["recurrence_key"], "master-1");
    }

    #[test]
    fn a_real_delta_reply_shape_deserializes() {
        let raw = r#"{
            "@odata.deltaLink": "https://graph.microsoft.com/v1.0/me/calendarView/delta?$deltatoken=abc",
            "value": [
                {
                    "id": "AAMk-1",
                    "subject": "Standup",
                    "start": { "dateTime": "2026-08-20T09:00:00.0000000", "timeZone": "UTC" },
                    "end": { "dateTime": "2026-08-20T09:30:00.0000000", "timeZone": "UTC" },
                    "isAllDay": false,
                    "isCancelled": false,
                    "attendees": []
                },
                { "id": "AAMk-2", "@removed": { "reason": "deleted" } }
            ]
        }"#;

        let reply: DeltaReply = serde_json::from_str(raw).expect("deserializes");
        assert_eq!(reply.value.len(), 2);
        assert!(reply.delta_link.is_some());
        assert!(to_inbound(&reply.value[0]).unwrap().is_some());
        assert!(to_inbound(&reply.value[1]).unwrap().is_none());
    }

    #[test]
    fn an_unknown_response_status_is_none_rather_than_a_guess() {
        assert_eq!(normalize_response(Some("notResponded")), "none");
        assert_eq!(normalize_response(None), "none");
    }
}
