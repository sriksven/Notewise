//! POST Notewise events to a URL the user controls.
//!
//! One connector that covers the automation long tail — Zapier, Make, n8n, or a script —
//! without a bespoke integration per destination.
//!
//! Deliveries are signed with HMAC-SHA256 over the raw body. A receiver otherwise has no way
//! to distinguish a real delivery from anything else that can reach its URL.

use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::connector::{Connector, SinkConnector};
use crate::credentials::Secret;
use crate::error::{ConnectorError, Result};
use crate::types::{ExternalRef, Health, Outbound};

pub const SIGNATURE_HEADER: &str = "X-Notewise-Signature";

/// Hex HMAC-SHA256 of `body` under `secret`.
pub(crate) fn sign(secret: &Secret, body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.expose().as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[derive(Debug)]
pub struct WebhookSink {
    url: String,
    secret: Secret,
    client: reqwest::Client,
}

impl WebhookSink {
    pub fn new(url: impl Into<String>, secret: Secret) -> Self {
        Self {
            url: url.into(),
            secret,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("default reqwest client builds"),
        }
    }
}

#[async_trait]
impl Connector for WebhookSink {
    fn id(&self) -> &str {
        "webhook"
    }

    fn display_name(&self) -> &str {
        "Webhook"
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn health(&self) -> Result<Health> {
        Ok(Health::Ok)
    }
}

#[async_trait]
impl SinkConnector for WebhookSink {
    async fn push(&self, outbound: &Outbound) -> Result<ExternalRef> {
        let envelope = serde_json::json!({
            "node_kind": outbound.node_kind,
            "node_id": outbound.node_id.to_string(),
            "operation": outbound.operation.as_str(),
            "data": outbound.payload,
        });
        let body = serde_json::to_string(&envelope)?;
        let signature = sign(&self.secret, &body);

        let response = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header(SIGNATURE_HEADER, &signature)
            .body(body)
            .send()
            .await
            .map_err(|e| ConnectorError::Transient(format!("request failed: {e}")))?;

        let status = response.status();
        if status.is_success() {
            return Ok(ExternalRef {
                external_id: signature,
                url: Some(self.url.clone()),
                title: None,
                remote_version: None,
            });
        }

        // Classification is what the dispatcher branches on, so each case is decided here
        // rather than left as a generic failure.
        Err(match status.as_u16() {
            401 | 403 => ConnectorError::Auth {
                connector: "webhook".into(),
            },
            429 => {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(60);
                ConnectorError::RateLimited {
                    retry_after: Duration::from_secs(retry_after),
                }
            }
            code if (500..600).contains(&code) => {
                ConnectorError::Transient(format!("receiver returned {code}"))
            }
            code => ConnectorError::Permanent(format!("receiver returned {code}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Operation;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::Router;
    use notewise_storage::Id;
    use std::sync::{Arc, Mutex};

    /// Start a test receiver that records what it was sent and replies with `status`.
    async fn receiver(status: u16) -> (String, Arc<Mutex<Vec<(HeaderMap, String)>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();

        let app = Router::new().route(
            "/hook",
            post(move |headers: HeaderMap, body: String| {
                let sink = sink.clone();
                async move {
                    sink.lock().unwrap().push((headers, body));
                    axum::http::StatusCode::from_u16(status).unwrap()
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        (format!("http://{addr}/hook"), received)
    }

    fn outbound() -> Outbound {
        Outbound {
            node_kind: "decision".into(),
            node_id: Id::new(),
            operation: Operation::Create,
            payload: serde_json::json!({"text": "Ship Friday"}),
            existing: None,
        }
    }

    #[tokio::test]
    async fn push_posts_the_payload() {
        let (url, received) = receiver(200).await;
        let sink = WebhookSink::new(url, Secret::new("shh"));

        sink.push(&outbound()).await.unwrap();

        let calls = received.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].1.contains("Ship Friday"));
    }

    #[tokio::test]
    async fn deliveries_are_signed_over_the_raw_body() {
        let (url, received) = receiver(200).await;
        let sink = WebhookSink::new(url, Secret::new("shh"));

        sink.push(&outbound()).await.unwrap();

        let calls = received.lock().unwrap();
        let (headers, body) = &calls[0];
        let signature = headers
            .get("x-notewise-signature")
            .expect("a receiver must be able to tell a real delivery from anything else that can reach its URL")
            .to_str()
            .unwrap();

        assert_eq!(signature, sign(&Secret::new("shh"), body));
    }

    #[tokio::test]
    async fn a_500_is_retryable() {
        let (url, _) = receiver(500).await;
        let sink = WebhookSink::new(url, Secret::new("shh"));

        let err = sink.push(&outbound()).await.unwrap_err();
        assert!(
            err.is_retryable(),
            "a receiver having a bad minute deserves another try"
        );
    }

    #[tokio::test]
    async fn a_400_is_permanent() {
        let (url, _) = receiver(400).await;
        let sink = WebhookSink::new(url, Secret::new("shh"));

        let err = sink.push(&outbound()).await.unwrap_err();
        assert!(
            !err.is_retryable(),
            "replaying a malformed request forever helps nobody"
        );
    }

    #[tokio::test]
    async fn a_401_is_an_auth_error() {
        let (url, _) = receiver(401).await;
        let sink = WebhookSink::new(url, Secret::new("shh"));

        let err = sink.push(&outbound()).await.unwrap_err();
        assert!(matches!(err, ConnectorError::Auth { .. }));
    }

    #[tokio::test]
    async fn a_429_is_rate_limited() {
        let (url, _) = receiver(429).await;
        let sink = WebhookSink::new(url, Secret::new("shh"));

        let err = sink.push(&outbound()).await.unwrap_err();
        assert!(err.is_retryable());
        assert!(matches!(err, ConnectorError::RateLimited { .. }));
    }

    #[test]
    fn the_webhook_is_not_local() {
        let sink = WebhookSink::new("https://example.com/hook", Secret::new("k"));
        assert!(!sink.is_local(), "a webhook sends data off the machine");
    }
}
