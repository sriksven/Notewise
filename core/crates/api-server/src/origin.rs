//! Only this machine may drive the engine — including the browser on it.
//!
//! [`crate::Server::bind`] keeps the network out: the listener is on loopback. That is not enough
//! on its own, because every web page the user opens runs *on this machine*. Two ways in, both
//! reproduced against a running engine before this existed:
//!
//! - **Cross-site requests.** A page cannot read a cross-origin response, but it can send one. A
//!   form post or a `fetch` with `Content-Type: text/plain` needs no preflight, and every handler
//!   taking `Option<Json<_>>` accepts a missing body — so any page could end a meeting, start a
//!   microphone recording in a capture build, or ask the OS for a permission.
//! - **DNS rebinding.** A page whose own hostname is re-pointed at 127.0.0.1 is *same-origin* with
//!   the engine, so it can read responses too: the meeting list, and `POST /v1/mcp/servers` with a
//!   `command` followed by `/start` is a program of the page's choosing run as the user.
//!
//! So every request must name a loopback host, which a rebinding page cannot do — its `Host` is its
//! own domain — and a request that changes anything must not come from a foreign origin. Clients
//! that legitimately reach the engine all pass: the desktop window is served by the engine itself
//! (`http://127.0.0.1:<port>`), the dev servers proxy from `localhost`, the browser extension's
//! worker sends `chrome-extension://<id>`, and the CLI, scripts and `curl` send no `Origin` at all.

use std::net::IpAddr;

use axum::extract::Request;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// Refuse a request from anywhere but this machine's own clients.
pub(crate) async fn guard(request: Request, next: Next) -> Response {
    // The `Host` header, and the URI's authority when the request carries one (HTTP/2, or the
    // absolute form of HTTP/1.1). Both must be loopback when present. An absent `Host` is allowed:
    // every browser sends one, so its absence means a client that is not a browser.
    let host = request.headers().get(header::HOST);
    let authority = request.uri().authority().map(|a| a.as_str());
    let host_ok = host.is_none_or(|value| value.to_str().is_ok_and(is_loopback_authority))
        && authority.is_none_or(is_loopback_authority);
    if !host_ok {
        return refuse(
            StatusCode::MISDIRECTED_REQUEST,
            "not_loopback",
            "this engine answers only requests addressed to 127.0.0.1, [::1] or localhost",
        );
    }

    if changes_state(request.method()) {
        if let Some(origin) = request.headers().get(header::ORIGIN) {
            if !is_trusted_origin(origin) {
                return refuse(
                    StatusCode::FORBIDDEN,
                    "foreign_origin",
                    "requests from web pages are not accepted; only Notewise's own window, its \
                     browser extension and local tools may change anything",
                );
            }
        }
    }

    next.run(request).await
}

/// Methods a browser lets a page send cross-origin without asking, plus the ones it would ask
/// about. Only reads are exempt — a page cannot see their answers without CORS, and there is none.
fn changes_state(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// `127.0.0.1:47821`, `[::1]:47821`, `localhost:1420`, or any of those without a port.
///
/// Any port, not only the one this engine is bound to. The Vite dev servers for the desktop app and
/// the dashboard proxy `/v1` without rewriting `Host`, so their requests arrive saying
/// `localhost:1420`. And the port adds nothing against rebinding: a rebinding page's `Host` is its
/// own domain, which fails here whatever port follows it.
fn is_loopback_authority(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        // A bracketed IPv6 literal, optionally followed by `:port`.
        match rest.split_once(']') {
            Some((inside, "")) => inside,
            Some((inside, after)) if after.strip_prefix(':').is_some_and(is_port) => inside,
            _ => return false,
        }
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) if is_port(port) => host,
            Some(_) => return false,
            None => authority,
        }
    };

    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn is_port(digits: &str) -> bool {
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// The origins allowed to change state: a loopback page, or a browser extension.
///
/// Extensions are allowed as a class. One with host permission for loopback can already reach the
/// engine in any way it likes, so refusing them would only break Notewise's own.
fn is_trusted_origin(origin: &HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };

    if let Some(id) = origin
        .strip_prefix("chrome-extension://")
        .or_else(|| origin.strip_prefix("moz-extension://"))
    {
        return !id.is_empty();
    }

    // `null` — a sandboxed frame, a `file:` page, a redirect — falls through to here and fails:
    // none of those is a client of this engine.
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .is_some_and(is_loopback_authority)
}

fn refuse(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    tracing::warn!(%code, "refused a request that did not come from this machine's clients");
    (
        status,
        Json(serde_json::json!({ "error": message, "code": code })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::{get, post};
    use tower::ServiceExt;

    fn app() -> axum::Router {
        axum::Router::new()
            .route("/read", get(|| async { "read" }))
            .route("/write", post(|| async { "wrote" }))
            .layer(axum::middleware::from_fn(guard))
    }

    async fn status(request: Request<Body>) -> StatusCode {
        app().oneshot(request).await.expect("response").status()
    }

    fn request(method: &str, uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::empty()).expect("request")
    }

    /// The desktop window, the dev proxy, the CLI and the extension all keep working.
    #[tokio::test]
    async fn this_machines_clients_are_served() {
        for headers in [
            vec![
                ("host", "127.0.0.1:47821"),
                ("origin", "http://127.0.0.1:47821"),
            ],
            vec![
                ("host", "localhost:1420"),
                ("origin", "http://localhost:1420"),
            ],
            vec![("host", "[::1]:47821")],
            vec![("host", "127.0.0.1:47821")],
            vec![
                ("host", "127.0.0.1:47822"),
                ("origin", "chrome-extension://abcdefghijklmnop"),
            ],
            vec![],
        ] {
            assert_eq!(
                status(request("POST", "/write", &headers)).await,
                StatusCode::OK,
                "{headers:?}"
            );
        }
    }

    /// A rebinding page addresses the engine by its own name. Reads included: same-origin to it,
    /// so it could read what came back.
    #[tokio::test]
    async fn a_foreign_host_is_refused_even_for_a_read() {
        for host in [
            "evil.example:47821",
            "evil.example",
            "127.0.0.1.evil.example:47821",
        ] {
            assert_eq!(
                status(request("GET", "/read", &[("host", host)])).await,
                StatusCode::MISDIRECTED_REQUEST,
                "{host}"
            );
        }
    }

    /// The cross-site request a page can send without a preflight.
    #[tokio::test]
    async fn a_write_from_a_web_page_is_refused() {
        for origin in [
            "https://evil.example",
            "null",
            "http://127.0.0.1.evil.example",
        ] {
            assert_eq!(
                status(request(
                    "POST",
                    "/write",
                    &[("host", "127.0.0.1:47821"), ("origin", origin)]
                ))
                .await,
                StatusCode::FORBIDDEN,
                "{origin}"
            );
        }
    }

    /// A read from another origin is left to the browser, which will not show the page the answer.
    #[tokio::test]
    async fn a_cross_origin_read_is_left_to_the_browser() {
        assert_eq!(
            status(request(
                "GET",
                "/read",
                &[
                    ("host", "127.0.0.1:47821"),
                    ("origin", "https://evil.example")
                ]
            ))
            .await,
            StatusCode::OK
        );
    }

    #[test]
    fn loopback_authorities_are_recognised() {
        for good in [
            "127.0.0.1",
            "127.0.0.1:1",
            "localhost",
            "LOCALHOST:80",
            "[::1]",
            "[::1]:47821",
        ] {
            assert!(is_loopback_authority(good), "{good}");
        }
        for bad in [
            "evil.example",
            "localhost.evil.example",
            "10.0.0.1:47821",
            "[::1]evil",
            "127.0.0.1:port",
            "127.0.0.1:",
            "",
        ] {
            assert!(!is_loopback_authority(bad), "{bad}");
        }
    }
}
