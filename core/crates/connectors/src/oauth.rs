//! The loopback leg of Microsoft's PKCE flow.
//!
//! # Why a listener and not a pasted code
//!
//! Microsoft's authorization code arrives as a query parameter on a redirect. The alternatives to
//! catching it are asking the user to copy a code out of a browser address bar — which is where
//! `urn:ietf:wg:oauth:2.0:oob` used to go before every vendor deprecated it — or registering a
//! custom URL scheme, which needs an installed, registered bundle and does not work for a `cargo
//! run`. A loopback listener is what the spec for native apps actually recommends, and it is the one
//! option that works for a development build.
//!
//! # What it accepts, and what it refuses
//!
//! One request, on a port the OS chose, for a state value generated moments earlier. Anything else
//! gets a page saying so and is not treated as the answer. That matters more than it looks: the
//! listener is briefly open to anything on the machine, and a code accepted without checking `state`
//! is the textbook cross-site request forgery for this flow.
//!
//! # Why it stops on its own
//!
//! A user who opens the consent page and closes the tab leaves this waiting. It has a deadline, and
//! reaching it is reported as "nobody finished signing in" rather than hanging a settings screen
//! forever.

use std::net::SocketAddr;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::credentials::Secret;
use crate::error::{ConnectorError, Result};
use crate::sources::{authorize_url, Pkce, SCOPES};

/// How long to wait for somebody to finish signing in.
///
/// Generous, because the bound is a person reading a consent screen and possibly a second factor.
/// Bounded, because a settings screen must not be stuck on a tab that was closed.
pub const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Where Microsoft redirects to.
///
/// Port zero: the OS picks a free one, which is what makes this work when a second Notewise is
/// already running. Microsoft allows any port on `http://localhost` for a public client precisely
/// so a native app does not have to reserve one.
const LOOPBACK: &str = "127.0.0.1:0";

/// A sign-in waiting for the user.
#[derive(Debug)]
pub struct PendingAuth {
    listener: TcpListener,
    pkce: Pkce,
    state: String,
    redirect_uri: String,
    authorize_url: String,
    /// How long to wait. A field rather than only a constant so a caller that knows better — and a
    /// test that must not wait five minutes — can say so.
    timeout: Duration,
}

impl PendingAuth {
    /// Open a listener and build the URL to send the user to.
    ///
    /// The listener is opened *before* the URL is returned, so the redirect cannot arrive at a port
    /// nothing is on — which is what would happen if the browser were opened first and the bind
    /// raced it.
    pub async fn start(client_id: &str) -> Result<Self> {
        let listener = TcpListener::bind(LOOPBACK)
            .await
            .map_err(|e| ConnectorError::Transient(format!("could not open a local port: {e}")))?;

        let addr: SocketAddr = listener.local_addr().map_err(|e| {
            ConnectorError::Transient(format!("could not read the local port: {e}"))
        })?;

        // `localhost` rather than `127.0.0.1`: Microsoft treats them as different redirect URIs, and
        // the loopback exemption is documented against `localhost`.
        let redirect_uri = format!("http://localhost:{}", addr.port());
        let pkce = Pkce::generate();
        let state = uuid::Uuid::new_v4().simple().to_string();
        let authorize_url = authorize_url(client_id, &redirect_uri, &pkce, &state);

        Ok(Self {
            listener,
            pkce,
            state,
            redirect_uri,
            authorize_url,
            timeout: CONSENT_TIMEOUT,
        })
    }

    /// Wait a different length of time.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Where to send the user.
    pub fn authorize_url(&self) -> &str {
        &self.authorize_url
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Wait for the redirect and return the authorization code.
    ///
    /// Requests that are not the one being waited for — a favicon fetch, a probe, a stale tab from a
    /// previous attempt — are answered and ignored rather than ending the wait. Treating the first
    /// connection as the answer would make a browser's speculative prefetch cancel a sign-in.
    pub async fn wait_for_code(&self) -> Result<String> {
        let deadline = tokio::time::Instant::now() + self.timeout;

        loop {
            let accepted = tokio::time::timeout_at(deadline, self.listener.accept()).await;

            let (mut stream, _) =
                match accepted {
                    Ok(Ok(pair)) => pair,
                    Ok(Err(e)) => {
                        return Err(ConnectorError::Transient(format!(
                            "the sign-in listener failed: {e}"
                        )))
                    }
                    Err(_) => return Err(ConnectorError::Transient(
                        "nobody finished signing in. Try again, and complete the page that opens."
                            .into(),
                    )),
                };

            let request = read_request(&mut stream).await.unwrap_or_default();

            match Callback::parse(&request, &self.state) {
                Ok(code) => {
                    respond(&mut stream, &success_page()).await;
                    return Ok(code);
                }
                Err(CallbackProblem::NotTheCallback) => {
                    // Something else on the machine, or the browser being helpful. Answered and
                    // ignored.
                    respond(&mut stream, &waiting_page()).await;
                }
                Err(CallbackProblem::Denied(reason)) => {
                    respond(&mut stream, &denied_page()).await;
                    return Err(ConnectorError::Auth {
                        connector: format!("microsoft ({reason})"),
                    });
                }
                Err(CallbackProblem::WrongState) => {
                    // A code for a sign-in nobody here started. Refused rather than exchanged.
                    respond(&mut stream, &mismatch_page()).await;
                    return Err(ConnectorError::Permanent(
                        "that sign-in did not match the one Notewise started; try again".into(),
                    ));
                }
            }
        }
    }

    /// Trade the code for a refresh token.
    pub async fn exchange(
        &self,
        client_id: &str,
        code: &str,
        token_url: &str,
        http: &reqwest::Client,
    ) -> Result<Secret> {
        let params = [
            ("client_id", client_id),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("code_verifier", self.pkce.verifier.as_str()),
            ("scope", SCOPES),
        ];

        let response = http
            .post(token_url)
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
            // An authorization code is single-use and short-lived, so a refusal here is not
            // something to retry — the user signs in again.
            return Err(ConnectorError::Permanent(format!(
                "Microsoft refused the sign-in ({status}). Try connecting again."
            )));
        }

        let reply: TokenReply = serde_json::from_str(&text)
            .map_err(|e| ConnectorError::Permanent(format!("unreadable token reply: {e}")))?;

        // Without `offline_access` there is no refresh token, and a connector holding only an access
        // token stops working in an hour with no way to recover. Refused rather than stored.
        reply.refresh_token.map(Secret::new).ok_or_else(|| {
            ConnectorError::Permanent(
                "Microsoft returned no refresh token, so Notewise could not stay connected. The \
                 offline_access permission was not granted."
                    .into(),
            )
        })
    }
}

#[derive(Debug, Deserialize)]
struct TokenReply {
    refresh_token: Option<String>,
}

/// Why a request on the listener was not the answer.
#[derive(Debug, PartialEq, Eq)]
pub enum CallbackProblem {
    /// Not a redirect from the sign-in at all.
    NotTheCallback,
    /// The user said no, or the provider did.
    Denied(String),
    /// A code arrived for a different sign-in.
    WrongState,
}

/// The redirect, parsed.
///
/// Pure and public so every branch is testable without a browser, a browser's prefetch, or a
/// vendor — which matters because the `state` check is the security property here and a test is the
/// only thing that can prove it is enforced.
#[derive(Debug)]
pub struct Callback;

impl Callback {
    pub fn parse(
        request: &str,
        expected_state: &str,
    ) -> std::result::Result<String, CallbackProblem> {
        let query = request_query(request).ok_or(CallbackProblem::NotTheCallback)?;
        let params = parse_query(&query);

        // An error takes precedence: a denial carries no code, and reporting "not the callback"
        // would leave the user waiting for something that is never coming.
        if let Some(error) = params.iter().find(|(k, _)| k == "error").map(|(_, v)| v) {
            let description = params
                .iter()
                .find(|(k, _)| k == "error_description")
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| error.clone());
            return Err(CallbackProblem::Denied(description));
        }

        let code = params
            .iter()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.clone())
            .ok_or(CallbackProblem::NotTheCallback)?;

        let state = params
            .iter()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        // Compared before the code is used for anything. A code accepted without this is the
        // textbook forgery for an authorization-code flow.
        if state != expected_state {
            return Err(CallbackProblem::WrongState);
        }

        Ok(code)
    }
}

/// The query string of an HTTP request line, if it has one.
fn request_query(request: &str) -> Option<String> {
    let line = request.lines().next()?;
    let mut parts = line.split_whitespace();

    // Only GET. A redirect is a GET, and anything else on this port is not the answer.
    if parts.next()? != "GET" {
        return None;
    }

    let target = parts.next()?;
    target.split_once('?').map(|(_, query)| query.to_string())
}

/// `a=1&b=2` into pairs, percent-decoded.
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

/// Percent-decoding, with `+` as a space.
///
/// Hand-rolled rather than a dependency: this decodes one query string once per sign-in, and the
/// alternative is pulling a URL crate into a file that would otherwise need nothing.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    // Not an escape after all. Kept literally rather than dropped, so a stray `%`
                    // in an error description does not eat the two characters after it.
                    Err(_) => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }

    String::from_utf8_lossy(&out).to_string()
}

/// Read enough of a request to find its first line.
///
/// One read of a bounded buffer. The request line and query are the only parts that matter, and a
/// client that sends a megabyte of headers to this port is not the browser being waited for.
async fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = [0u8; 4096];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
        .await
        .ok()?
        .ok()?;

    Some(String::from_utf8_lossy(&buffer[..read]).to_string())
}

/// Answer with a page and close.
async fn respond(stream: &mut TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );

    // Best effort: the code is already in hand, and a browser that hung up before reading the page
    // costs the user a blank tab rather than a failed sign-in.
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

fn page(heading: &str, detail: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>Notewise</title>\
         <body style=\"font:16px -apple-system,system-ui,sans-serif;margin:15vh auto;max-width:28rem;\
         text-align:center;color:#222\"><h1 style=\"font-size:1.25rem\">{heading}</h1>\
         <p style=\"color:#666;line-height:1.5\">{detail}</p></body>"
    )
}

/// What the browser shows once the code is in hand.
fn success_page() -> String {
    page("Connected", "You can close this tab and go back to Notewise.")
}

/// Shown to anything that reaches this port before the redirect does — a favicon fetch, usually.
fn waiting_page() -> String {
    page(
        "Still waiting",
        "This page is where Microsoft will send you after you sign in. Nothing to do here yet.",
    )
}

fn denied_page() -> String {
    page(
        "Not connected",
        "Microsoft did not grant access. You can close this tab and try again from Notewise.",
    )
}

fn mismatch_page() -> String {
    page(
        "That did not match",
        "This sign-in was not the one Notewise started, so it was refused. Start again from \
         Notewise.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_request(target: &str) -> String {
        format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n")
    }

    /// The happy path, and the only one that yields a code.
    #[test]
    fn a_matching_callback_yields_its_code() {
        let request = get_request("/?code=abc123&state=s1");
        assert_eq!(Callback::parse(&request, "s1"), Ok("abc123".to_string()));
    }

    /// The security property. A code accepted without this is the textbook forgery for this flow.
    #[test]
    fn a_code_for_another_sign_in_is_refused() {
        let request = get_request("/?code=abc123&state=somebody-elses");
        assert_eq!(
            Callback::parse(&request, "ours"),
            Err(CallbackProblem::WrongState)
        );
    }

    /// A code with no state at all is not the one we started either.
    #[test]
    fn a_callback_with_no_state_is_refused() {
        let request = get_request("/?code=abc123");
        assert_eq!(
            Callback::parse(&request, "ours"),
            Err(CallbackProblem::WrongState)
        );
    }

    /// A denial carries no code, and reporting "not the callback" would leave the user waiting for
    /// something that is never coming.
    #[test]
    fn a_denial_is_reported_with_what_the_provider_said() {
        let request = get_request(
            "/?error=access_denied&error_description=The+user+cancelled+the+request&state=s1",
        );
        match Callback::parse(&request, "s1") {
            Err(CallbackProblem::Denied(reason)) => {
                assert_eq!(reason, "The user cancelled the request")
            }
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    /// A denial is a denial even when the state does not match: there is no code to protect.
    #[test]
    fn a_denial_takes_precedence_over_the_state_check() {
        let request = get_request("/?error=access_denied&state=whatever");
        assert!(matches!(
            Callback::parse(&request, "ours"),
            Err(CallbackProblem::Denied(_))
        ));
    }

    /// A browser's speculative prefetch must not cancel a sign-in.
    #[test]
    fn a_favicon_request_is_not_the_callback() {
        assert_eq!(
            Callback::parse(&get_request("/favicon.ico"), "s1"),
            Err(CallbackProblem::NotTheCallback)
        );
        assert_eq!(
            Callback::parse(&get_request("/"), "s1"),
            Err(CallbackProblem::NotTheCallback)
        );
    }

    /// A redirect is a GET. Anything else on this port is not the answer.
    #[test]
    fn a_post_is_not_the_callback() {
        let request = "POST /?code=abc&state=s1 HTTP/1.1\r\n\r\n";
        assert_eq!(
            Callback::parse(request, "s1"),
            Err(CallbackProblem::NotTheCallback)
        );
    }

    #[test]
    fn nonsense_is_not_the_callback() {
        for request in ["", "\r\n\r\n", "hello", "GET"] {
            assert_eq!(
                Callback::parse(request, "s1"),
                Err(CallbackProblem::NotTheCallback),
                "{request:?}"
            );
        }
    }

    /// Codes contain characters that get percent-escaped, and a mangled code fails the exchange with
    /// an error that says nothing about why.
    #[test]
    fn an_escaped_code_is_decoded() {
        let request = get_request("/?code=a%2Fb%2Bc%3Dd&state=s1");
        assert_eq!(Callback::parse(&request, "s1"), Ok("a/b+c=d".to_string()));
    }

    #[test]
    fn a_plus_in_a_description_is_a_space() {
        assert_eq!(percent_decode("one+two"), "one two");
        assert_eq!(percent_decode("caf%C3%A9"), "café");
    }

    /// A stray `%` must not eat the two characters after it.
    #[test]
    fn a_stray_percent_is_kept_literally() {
        assert_eq!(percent_decode("100%ok"), "100%ok");
        assert_eq!(percent_decode("50%"), "50%");
    }

    /// The listener opens before the URL is handed out, so the redirect cannot arrive at a port
    /// nothing is on. Runs, because it needs no vendor.
    #[tokio::test]
    async fn a_pending_sign_in_is_listening_before_it_says_where_to_go() {
        let pending = PendingAuth::start("client-id").await.expect("starts");

        let uri = pending.redirect_uri().to_string();
        let port: u16 = uri
            .rsplit(':')
            .next()
            .expect("a port")
            .parse()
            .expect("a number");
        assert_ne!(port, 0, "the OS must have chosen a real port");

        // Something is actually accepting there.
        let connected = tokio::net::TcpStream::connect(("127.0.0.1", port)).await;
        assert!(connected.is_ok(), "{connected:?}");

        assert!(pending.authorize_url().contains("code_challenge="));
        assert!(pending.authorize_url().contains(&urlencode(&uri)));
    }

    /// The whole loopback leg, driven by a fake browser. No vendor, no consent screen.
    #[tokio::test]
    async fn the_code_arrives_from_the_redirect() {
        let pending = PendingAuth::start("client-id").await.expect("starts");
        let uri = pending.redirect_uri().to_string();
        let state = pending.state.clone();

        // A browser that first prefetches, then follows the redirect — the sequence that made
        // "treat the first connection as the answer" wrong.
        tokio::spawn(async move {
            let port: u16 = uri.rsplit(':').next().unwrap().parse().unwrap();

            for target in [
                "/favicon.ico".to_string(),
                format!("/?code=the-code&state={state}"),
            ] {
                if let Ok(mut stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                    let _ = stream
                        .write_all(format!("GET {target} HTTP/1.1\r\n\r\n").as_bytes())
                        .await;
                    let mut sink = Vec::new();
                    let _ = stream.read_to_end(&mut sink).await;
                }
            }
        });

        let code = pending.wait_for_code().await.expect("a code");
        assert_eq!(code, "the-code");
    }

    /// A closed tab must not hang a settings screen forever.
    #[tokio::test]
    async fn nobody_signing_in_times_out_with_a_message_worth_showing() {
        let pending = PendingAuth::start("client-id")
            .await
            .expect("starts")
            .with_timeout(Duration::from_millis(50));

        let error = pending.wait_for_code().await.expect_err("must give up");
        assert!(error.to_string().contains("finished signing in"), "{error}");
    }

    /// Percent-encode, for asserting the redirect made it into the URL.
    fn urlencode(value: &str) -> String {
        value
            .chars()
            .map(|c| match c {
                'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
                other => format!("%{:02X}", other as u32),
            })
            .collect()
    }

    #[test]
    fn the_page_helper_produces_something_a_browser_can_render() {
        let rendered = page("Connected", "Close this tab.");
        assert!(rendered.starts_with("<!doctype html>"));
        assert!(rendered.contains("Connected"));
    }
}
