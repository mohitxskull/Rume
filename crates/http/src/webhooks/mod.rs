//! Inbound webhook ingestion and HMAC-SHA256 signature + replay verification middleware.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, HeaderName, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use hmac::{Hmac, KeyInit, Mac};
use secrecy::{ExposeSecret, SecretString};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tower::{Layer, Service};

type HmacSha256 = Hmac<Sha256>;

/// Resolves the HMAC signing secret for an inbound webhook request.
pub trait WebhookSecretResolver: Send + Sync + 'static {
    /// Resolves the shared secret for the incoming request headers, or `None` if unknown.
    fn resolve_secret<'a>(
        &'a self,
        headers: &'a HeaderMap,
    ) -> Pin<Box<dyn Future<Output = Option<SecretString>> + Send + 'a>>;
}

/// Static [`WebhookSecretResolver`] that always returns the same configured secret.
#[derive(Debug, Clone)]
pub struct StaticWebhookSecretResolver {
    secret: SecretString,
}

impl StaticWebhookSecretResolver {
    /// Creates a new [`StaticWebhookSecretResolver`].
    pub fn new(secret: SecretString) -> Self {
        Self { secret }
    }
}

impl WebhookSecretResolver for StaticWebhookSecretResolver {
    fn resolve_secret<'a>(
        &'a self,
        _headers: &'a HeaderMap,
    ) -> Pin<Box<dyn Future<Output = Option<SecretString>> + Send + 'a>> {
        let secret = self.secret.clone();
        Box::pin(async move { Some(secret) })
    }
}

/// Configuration for [`VerifyWebhookSignatureLayer`].
#[derive(Clone)]
pub struct WebhookSignatureConfig {
    /// Resolver for the endpoint's HMAC-SHA256 secret.
    pub secret_resolver: Arc<dyn WebhookSecretResolver>,
    /// Maximum allowed age (or future clock skew) for the `t=` timestamp. Default: 300s.
    pub max_age: Duration,
    /// Maximum allowed request body size in bytes. Default: 1 MiB.
    pub max_body_bytes: usize,
    /// Header carrying the signature (`t=<ts>,v1=<hex>`). Default: `X-Rume-Signature`.
    pub header_name: HeaderName,
}

impl WebhookSignatureConfig {
    /// Creates a [`WebhookSignatureConfig`] with default `max_age` (300s),
    /// `max_body_bytes` (1 MiB), and `header_name` (`X-Rume-Signature`).
    pub fn new(secret_resolver: Arc<dyn WebhookSecretResolver>) -> Self {
        Self {
            secret_resolver,
            max_age: Duration::from_secs(300),
            max_body_bytes: 1024 * 1024,
            header_name: HeaderName::from_static("x-rume-signature"),
        }
    }

    /// Convenience constructor for a single static signing secret.
    pub fn from_static_secret(secret: SecretString) -> Self {
        Self::new(Arc::new(StaticWebhookSecretResolver::new(secret)))
    }

    /// Sets the maximum allowed timestamp age/skew.
    pub fn with_max_age(mut self, max_age: Duration) -> Self {
        self.max_age = max_age;
        self
    }

    /// Sets the maximum allowed request body size in bytes.
    pub fn with_max_body_bytes(mut self, max_body_bytes: usize) -> Self {
        self.max_body_bytes = max_body_bytes;
        self
    }

    /// Sets the signature header name.
    pub fn with_header_name(mut self, header_name: HeaderName) -> Self {
        self.header_name = header_name;
        self
    }
}

/// Tower/Axum middleware layer verifying `X-Rume-Signature` HMAC-SHA256 and timestamp freshness.
#[derive(Clone)]
pub struct VerifyWebhookSignatureLayer {
    config: WebhookSignatureConfig,
}

impl VerifyWebhookSignatureLayer {
    /// Creates a new [`VerifyWebhookSignatureLayer`].
    pub fn new(config: WebhookSignatureConfig) -> Self {
        Self { config }
    }
}

impl<S> Layer<S> for VerifyWebhookSignatureLayer {
    type Service = VerifyWebhookSignatureService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        VerifyWebhookSignatureService {
            inner,
            config: self.config.clone(),
        }
    }
}

/// Tower service produced by [`VerifyWebhookSignatureLayer`].
#[derive(Clone)]
pub struct VerifyWebhookSignatureService<S> {
    inner: S,
    config: WebhookSignatureConfig,
}

fn parse_signature_header(header_val: &str) -> Option<(u64, [u8; 32])> {
    let mut timestamp: Option<u64> = None;
    let mut sig_bytes: Option<[u8; 32]> = None;

    for part in header_val.split(',') {
        let part = part.trim();
        if let Some(ts_str) = part.strip_prefix("t=") {
            timestamp = ts_str.parse::<u64>().ok();
        } else if let Some(v1_hex) = part.strip_prefix("v1=") {
            let decoded = hex::decode(v1_hex).ok()?;
            if decoded.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&decoded);
                sig_bytes = Some(arr);
            }
        }
    }

    match (timestamp, sig_bytes) {
        (Some(ts), Some(sig)) => Some((ts, sig)),
        _ => None,
    }
}

impl<S> Service<Request<Body>> for VerifyWebhookSignatureService<S>
where
    S: Service<Request<Body>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        let config = self.config.clone();

        Box::pin(async move {
            let (parts, body) = req.into_parts();

            let Some(raw_header) = parts
                .headers
                .get(&config.header_name)
                .and_then(|v| v.to_str().ok())
            else {
                return Ok((StatusCode::UNAUTHORIZED, "missing webhook signature").into_response());
            };

            let Some((ts, provided_sig)) = parse_signature_header(raw_header) else {
                return Ok(
                    (StatusCode::UNAUTHORIZED, "invalid webhook signature format").into_response(),
                );
            };

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if now.abs_diff(ts) > config.max_age.as_secs() {
                return Ok((
                    StatusCode::UNAUTHORIZED,
                    "webhook signature timestamp expired",
                )
                    .into_response());
            }

            let Some(secret) = config.secret_resolver.resolve_secret(&parts.headers).await else {
                return Ok((StatusCode::UNAUTHORIZED, "unknown webhook secret").into_response());
            };

            let Ok(bytes) = to_bytes(body, config.max_body_bytes).await else {
                return Ok(
                    (StatusCode::PAYLOAD_TOO_LARGE, "webhook payload too large").into_response()
                );
            };

            let mut mac = HmacSha256::new_from_slice(secret.expose_secret().as_bytes())
                .expect("HMAC-SHA256 accepts keys of any length");
            mac.update(ts.to_string().as_bytes());
            mac.update(b".");
            mac.update(&bytes);
            let expected: [u8; 32] = mac.finalize().into_bytes().into();

            if !bool::from(expected.ct_eq(&provided_sig)) {
                return Ok((StatusCode::UNAUTHORIZED, "invalid webhook signature").into_response());
            }

            let reconstructed = Request::from_parts(parts, Body::from(bytes));
            inner.call(reconstructed).await
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use rume_messaging::webhooks::sign_webhook_payload;
    use secrecy::SecretString;
    use tower::ServiceExt;

    use super::*;

    async fn ok_echo(body: axum::body::Bytes) -> (StatusCode, axum::body::Bytes) {
        (StatusCode::OK, body)
    }

    #[tokio::test]
    async fn inbound_webhook_middleware_accepts_valid_signature_and_rejects_tampered_or_replayed() {
        let secret = SecretString::from("whsec_inbound_secret".to_string());
        let config = WebhookSignatureConfig::from_static_secret(secret.clone())
            .with_max_age(Duration::from_secs(300));
        let app = Router::new()
            .route("/webhook", post(ok_echo))
            .layer(VerifyWebhookSignatureLayer::new(config));

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let payload = br#"{"event":"invoice.paid"}"#;
        let valid_sig = sign_webhook_payload(&secret, now, payload);

        // 1. Valid signature -> 200 OK with intact body
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook")
                    .header("X-Rume-Signature", &valid_sig)
                    .body(Body::from(payload.as_slice()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], payload);

        // 2. Tampered body -> 401 Unauthorized
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook")
                    .header("X-Rume-Signature", &valid_sig)
                    .body(Body::from(br#"{"event":"invoice.tampered"}"#.as_slice()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // 3. Expired timestamp -> 401 Unauthorized
        let expired_ts = now.saturating_sub(600);
        let expired_sig = sign_webhook_payload(&secret, expired_ts, payload);
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook")
                    .header("X-Rume-Signature", &expired_sig)
                    .body(Body::from(payload.as_slice()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn inbound_webhook_middleware_rejects_oversized_body_with_413() {
        let secret = SecretString::from("whsec_inbound_secret".to_string());
        let config =
            WebhookSignatureConfig::from_static_secret(secret.clone()).with_max_body_bytes(16);
        let app = Router::new()
            .route("/webhook", post(ok_echo))
            .layer(VerifyWebhookSignatureLayer::new(config));

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let payload = b"this payload is definitely longer than sixteen bytes";
        let sig = sign_webhook_payload(&secret, now, payload);

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook")
                    .header("X-Rume-Signature", &sig)
                    .body(Body::from(payload.as_slice()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
