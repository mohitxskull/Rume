//! Outbound webhook dispatcher, HTTP transport, and Outbox/JobQueue integrations.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::Utc;
use rume_foundation::ssrf::{is_forbidden_ip, validate_host};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::signer::sign_webhook_payload;
use super::store::{WebhookDeliveryRecord, WebhookDeliveryStatus, WebhookDeliveryStore};
use super::{WebhookEndpoint, WebhookError};
use crate::jobs::{Job, JobContext, JobError, JobHandler, RetryPolicy};
use crate::outbox::error::OutboxError;
use crate::outbox::message::OutboxMessage;
use crate::outbox::repository::BoxFuture;

const DEFAULT_WEBHOOK_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_WEBHOOK_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_WEBHOOK_CLAIM_LEASE: Duration = Duration::from_secs(60);
const MAX_WEBHOOK_RESPONSE_BYTES: usize = 4096;

fn validate_webhook_url(
    url: &str,
    allow_private_networks: bool,
) -> Result<reqwest::Url, WebhookError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| WebhookError::Transport(format!("invalid webhook URL: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(WebhookError::Transport(format!(
            "unsupported webhook URL scheme '{}'",
            parsed.scheme()
        )));
    }
    let Some(host) = parsed.host_str() else {
        return Err(WebhookError::Transport("webhook URL missing host".into()));
    };
    if !allow_private_networks {
        validate_host(host).map_err(|reason| {
            WebhookError::Transport(format!("SSRF protection blocked webhook target: {reason}"))
        })?;
    }
    Ok(parsed)
}

#[derive(Debug)]
struct SsrfSafeDnsResolver;

impl reqwest::dns::Resolve for SsrfSafeDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                .collect();
            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::AddrNotAvailable,
                    format!("DNS resolution returned no addresses for '{host}'"),
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            for addr in &addrs {
                if is_forbidden_ip(addr.ip()) {
                    return Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!(
                            "SSRF protection blocked resolved private/internal IP {} for '{host}'",
                            addr.ip()
                        ),
                    ))
                        as Box<dyn std::error::Error + Send + Sync>);
                }
            }
            let iter: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

fn build_webhook_client(
    connect_timeout: Duration,
    timeout: Duration,
    allow_private_networks: bool,
) -> Result<reqwest::Client, WebhookError> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(connect_timeout)
        .timeout(timeout);
    if !allow_private_networks {
        builder = builder.dns_resolver(Arc::new(SsrfSafeDnsResolver));
    }
    builder
        .build()
        .map_err(|e| WebhookError::Transport(format!("failed to build webhook client: {e}")))
}

/// Hardened builder for [`ReqwestWebhookTransport`].
///
/// Why: exposing an arbitrary `with_client(reqwest::Client)` would allow callers to bypass
/// `SsrfSafeDnsResolver` and `Policy::none()`. This builder lets callers tune timeouts and
/// private-network opt-in while always attaching the hardened redirect and DNS policies.
#[derive(Debug, Clone)]
pub struct ReqwestWebhookTransportBuilder {
    connect_timeout: Duration,
    timeout: Duration,
    allow_private_networks: bool,
}

impl Default for ReqwestWebhookTransportBuilder {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_WEBHOOK_CONNECT_TIMEOUT,
            timeout: DEFAULT_WEBHOOK_TIMEOUT,
            allow_private_networks: false,
        }
    }
}

impl ReqwestWebhookTransportBuilder {
    /// Sets the TCP connect timeout.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Sets the total per-request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Configures whether private, loopback, and link-local network targets are permitted.
    ///
    /// Defaults to `false`. Enable only for local integration tests.
    pub fn allow_private_networks(mut self, allow: bool) -> Self {
        self.allow_private_networks = allow;
        self
    }

    /// Builds a hardened [`ReqwestWebhookTransport`].
    pub fn build(self) -> Result<ReqwestWebhookTransport, WebhookError> {
        let client = build_webhook_client(
            self.connect_timeout,
            self.timeout,
            self.allow_private_networks,
        )?;
        Ok(ReqwestWebhookTransport {
            client,
            allow_private_networks: self.allow_private_networks,
        })
    }
}

/// HTTP response returned by a [`WebhookTransport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookHttpResponse {
    /// HTTP status code returned by the webhook receiver.
    pub status: u16,
    /// Optional response body snippet.
    pub body: Option<String>,
}

/// Transport abstraction for sending signed outbound webhook HTTP requests.
pub trait WebhookTransport: std::fmt::Debug + Send + Sync + 'static {
    /// Sends a signed POST request to `url` with `X-Rume-Signature` and metadata headers.
    fn send<'a>(
        &'a self,
        url: &'a str,
        signature_header: &'a str,
        event_type: &'a str,
        delivery_id: &'a str,
        body: Vec<u8>,
    ) -> BoxFuture<'a, Result<WebhookHttpResponse, WebhookError>>;
}

/// Default [`WebhookTransport`] backed by `reqwest::Client`.
#[derive(Debug, Clone)]
pub struct ReqwestWebhookTransport {
    client: reqwest::Client,
    allow_private_networks: bool,
}

impl Default for ReqwestWebhookTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestWebhookTransport {
    /// Creates a new [`ReqwestWebhookTransport`] with SSRF protection, disabled redirects,
    /// and bounded connect/request timeouts.
    pub fn new() -> Self {
        Self::builder()
            .build()
            .expect("valid default reqwest webhook client config")
    }

    /// Returns a hardened [`ReqwestWebhookTransportBuilder`].
    pub fn builder() -> ReqwestWebhookTransportBuilder {
        ReqwestWebhookTransportBuilder::default()
    }

    /// Configures whether private, loopback, and link-local network targets are permitted.
    ///
    /// Defaults to `false`. Enable only for local integration tests.
    pub fn with_allow_private_networks(self, allow: bool) -> Self {
        Self::builder()
            .allow_private_networks(allow)
            .build()
            .expect("valid reqwest webhook client config")
    }
}

impl WebhookTransport for ReqwestWebhookTransport {
    fn send<'a>(
        &'a self,
        url: &'a str,
        signature_header: &'a str,
        event_type: &'a str,
        delivery_id: &'a str,
        body: Vec<u8>,
    ) -> BoxFuture<'a, Result<WebhookHttpResponse, WebhookError>> {
        Box::pin(async move {
            let parsed_url = validate_webhook_url(url, self.allow_private_networks)?;
            let mut response = self
                .client
                .post(parsed_url)
                .header("Content-Type", "application/json")
                .header("X-Rume-Signature", signature_header)
                .header("X-Rume-Event", event_type)
                .header("X-Rume-Delivery", delivery_id)
                .body(body)
                .send()
                .await
                .map_err(|e| WebhookError::Transport(e.to_string()))?;

            let status = response.status();
            if status.is_redirection() {
                return Err(WebhookError::Transport(format!(
                    "webhook redirect rejected: HTTP {}",
                    status.as_u16()
                )));
            }

            let mut buf = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| WebhookError::Transport(e.to_string()))?
            {
                let remaining = MAX_WEBHOOK_RESPONSE_BYTES.saturating_sub(buf.len());
                if remaining == 0 {
                    break;
                }
                let take = chunk.len().min(remaining);
                buf.extend_from_slice(&chunk[..take]);
                if buf.len() >= MAX_WEBHOOK_RESPONSE_BYTES {
                    break;
                }
            }
            let text = Some(String::from_utf8_lossy(&buf).into_owned());
            Ok(WebhookHttpResponse {
                status: status.as_u16(),
                body: text,
            })
        })
    }
}

/// Dispatches signed outbound webhooks with exponential backoff and delivery logging.
#[derive(Debug, Clone)]
pub struct WebhookDispatcher {
    endpoints: Arc<RwLock<HashMap<String, WebhookEndpoint>>>,
    store: Arc<dyn WebhookDeliveryStore>,
    transport: Arc<dyn WebhookTransport>,
    retry_policy: RetryPolicy,
}

impl WebhookDispatcher {
    /// Creates a new [`WebhookDispatcher`].
    pub fn new(store: Arc<dyn WebhookDeliveryStore>, transport: Arc<dyn WebhookTransport>) -> Self {
        Self {
            endpoints: Arc::new(RwLock::new(HashMap::new())),
            store,
            transport,
            retry_policy: RetryPolicy::default(),
        }
    }

    /// Configures the [`RetryPolicy`] used for failed or non-2xx deliveries.
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Registers an outbound webhook endpoint.
    pub fn register_endpoint(&self, endpoint: WebhookEndpoint) {
        if let Ok(mut guard) = self.endpoints.write() {
            guard.insert(endpoint.id.clone(), endpoint);
        }
    }

    fn matching_endpoints(
        &self,
        tenant_id: Option<&str>,
        event_type: &str,
    ) -> Vec<WebhookEndpoint> {
        let Ok(guard) = self.endpoints.read() else {
            return Vec::new();
        };
        guard
            .values()
            .filter(|ep| ep.tenant_id.as_deref() == tenant_id && ep.subscribes_to(event_type))
            .cloned()
            .collect()
    }

    fn endpoint_by_id(&self, id: &str) -> Option<WebhookEndpoint> {
        self.endpoints.read().ok()?.get(id).cloned()
    }

    /// Dispatches `event_type` with `payload` to all active matching endpoints scoped to `tenant_id`.
    pub async fn dispatch_event(
        &self,
        tenant_id: Option<&str>,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<Vec<WebhookDeliveryRecord>, WebhookError> {
        let payload_str = serde_json::to_string(&payload)
            .map_err(|e| WebhookError::Serialization(e.to_string()))?;
        let endpoints = self.matching_endpoints(tenant_id, event_type);
        let mut results = Vec::with_capacity(endpoints.len());

        for endpoint in endpoints {
            let now = Utc::now();
            let lease_until =
                now + chrono::Duration::from_std(DEFAULT_WEBHOOK_CLAIM_LEASE).unwrap_or_default();
            let mut record = WebhookDeliveryRecord {
                id: Uuid::new_v4().to_string(),
                endpoint_id: endpoint.id.clone(),
                event_type: event_type.to_string(),
                payload: payload_str.clone(),
                status: WebhookDeliveryStatus::InFlight,
                attempt: 0,
                next_attempt_at: Some(lease_until),
                last_response: None,
                created_at: now,
                updated_at: now,
            };
            self.store.upsert(record.clone()).await?;
            self.attempt_delivery(&endpoint, &mut record).await?;
            results.push(record);
        }

        Ok(results)
    }

    /// Processes due pending deliveries up to `limit`, returning the count attempted.
    pub async fn process_due_deliveries(&self, limit: usize) -> Result<usize, WebhookError> {
        let due = self
            .store
            .claim_due(Utc::now(), limit, DEFAULT_WEBHOOK_CLAIM_LEASE)
            .await?;
        let mut attempted = 0;

        for mut record in due {
            let Some(endpoint) = self.endpoint_by_id(&record.endpoint_id) else {
                record.status = WebhookDeliveryStatus::Failed;
                record.next_attempt_at = None;
                record.last_response = Some("endpoint not registered".to_string());
                record.updated_at = Utc::now();
                self.store.upsert(record).await?;
                continue;
            };
            self.attempt_delivery(&endpoint, &mut record).await?;
            attempted += 1;
        }

        Ok(attempted)
    }

    async fn attempt_delivery(
        &self,
        endpoint: &WebhookEndpoint,
        record: &mut WebhookDeliveryRecord,
    ) -> Result<(), WebhookError> {
        let now = Utc::now();
        let ts = u64::try_from(now.timestamp().max(0)).unwrap_or(0);
        let body = record.payload.as_bytes().to_vec();
        let sig = sign_webhook_payload(&endpoint.secret, ts, &body);

        let send_res = self
            .transport
            .send(&endpoint.url, &sig, &record.event_type, &record.id, body)
            .await;

        record.attempt = record.attempt.saturating_add(1);
        record.updated_at = Utc::now();

        match send_res {
            Ok(resp) if (200..300).contains(&resp.status) => {
                record.status = WebhookDeliveryStatus::Delivered;
                record.next_attempt_at = None;
                record.last_response = Some(format!("HTTP {}", resp.status));
            }
            Ok(resp) => {
                record.last_response = Some(format!("HTTP {}", resp.status));
                self.schedule_retry_or_fail(record);
            }
            Err(err) => {
                record.last_response = Some(err.to_string());
                self.schedule_retry_or_fail(record);
            }
        }

        self.store.upsert(record.clone()).await
    }

    fn schedule_retry_or_fail(&self, record: &mut WebhookDeliveryRecord) {
        if let Some(delay) = self.retry_policy.next_backoff(record.attempt) {
            let delta = chrono::Duration::from_std(delay).unwrap_or_default();
            record.status = WebhookDeliveryStatus::Pending;
            record.next_attempt_at = Some(Utc::now() + delta);
        } else {
            record.status = WebhookDeliveryStatus::Failed;
            record.next_attempt_at = None;
        }
    }

    /// Builds an [`OutboxRelay`](crate::outbox::OutboxRelay) dispatcher closure that forwards
    /// outbox messages to this [`WebhookDispatcher`].
    pub fn outbox_dispatcher(
        dispatcher: Arc<WebhookDispatcher>,
    ) -> impl Fn(&OutboxMessage) -> BoxFuture<'static, Result<(), OutboxError>> + Send + Sync + 'static
    {
        move |msg: &OutboxMessage| {
            let dispatcher = Arc::clone(&dispatcher);
            let event_type = msg.event_type.clone();
            let payload_bytes = msg.payload.clone();
            Box::pin(async move {
                let payload: serde_json::Value = serde_json::from_slice(&payload_bytes)
                    .map_err(|e| OutboxError::Dispatch(format!("invalid JSON payload: {e}")))?;
                dispatcher
                    .dispatch_event(None, &event_type, payload)
                    .await
                    .map(|_| ())
                    .map_err(|e| OutboxError::Dispatch(e.to_string()))
            })
        }
    }
}

/// Background [`Job`] payload for dispatching a webhook event via [`JobQueue`](crate::jobs::JobQueue).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookDeliveryJob {
    /// Optional tenant scope for multi-tenant webhook isolation.
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// Event type name.
    pub event_type: String,
    /// JSON payload to deliver.
    pub payload: serde_json::Value,
}

impl Job for WebhookDeliveryJob {
    const NAME: &'static str = "rume.webhook.deliver";
}

/// [`JobHandler`] that executes [`WebhookDeliveryJob`] via a [`WebhookDispatcher`].
#[derive(Debug, Clone)]
pub struct WebhookDeliveryJobHandler {
    dispatcher: Arc<WebhookDispatcher>,
}

impl WebhookDeliveryJobHandler {
    /// Creates a new [`WebhookDeliveryJobHandler`].
    pub fn new(dispatcher: Arc<WebhookDispatcher>) -> Self {
        Self { dispatcher }
    }
}

impl JobHandler<WebhookDeliveryJob> for WebhookDeliveryJobHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        job: WebhookDeliveryJob,
    ) -> crate::jobs::BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            self.dispatcher
                .dispatch_event(job.tenant_id.as_deref(), &job.event_type, job.payload)
                .await
                .map(|_| ())
                .map_err(|e| JobError::Retryable(e.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use secrecy::SecretString;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::webhooks::InMemoryWebhookDeliveryStore;

    async fn spawn_raw_http_server(response: String) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = Arc::clone(&hits);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                hits_clone.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (format!("http://{addr}"), hits)
    }

    #[tokio::test]
    async fn reqwest_webhook_transport_rejects_ssrf_ips_by_default() {
        let transport = ReqwestWebhookTransport::new();
        for blocked in [
            "http://127.0.0.1:8080/hook",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.1/internal",
            "http://192.168.1.1/admin",
            "http://[::1]:8080/hook",
            "http://[fe80::1]/hook",
            "http://[fd00::1]/hook",
            "http://[::ffff:127.0.0.1]/hook",
            "http://[64:ff9b::a9fe:a9fe]/hook",
            "http://[::127.0.0.1]/hook",
            "http://0.1.2.3/hook",
            "http://198.18.0.1/hook",
            "https://localhost./hook",
            "http://localhost:9000/hook",
        ] {
            let err = transport
                .send(blocked, "t=1,v1=abc", "evt", "del-1", b"{}".to_vec())
                .await
                .expect_err(&format!("expected SSRF rejection for {blocked}"));
            assert!(
                matches!(err, WebhookError::Transport(ref msg) if msg.contains("SSRF")),
                "unexpected error for {blocked}: {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn reqwest_webhook_transport_rejects_redirects_without_following() {
        let (target_url, target_hits) =
            spawn_raw_http_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".into()).await;
        let redirect_resp = format!(
            "HTTP/1.1 302 Found\r\nLocation: {target_url}/metadata\r\nContent-Length: 0\r\n\r\n"
        );
        let (redir_url, redir_hits) = spawn_raw_http_server(redirect_resp).await;

        let transport = ReqwestWebhookTransport::new().with_allow_private_networks(true);
        let err = transport
            .send(&redir_url, "t=1,v1=abc", "evt", "del-1", b"{}".to_vec())
            .await
            .expect_err("expected 302 redirect to be rejected");
        assert!(matches!(err, WebhookError::Transport(ref msg) if msg.contains("redirect")));
        assert_eq!(redir_hits.load(Ordering::SeqCst), 1);
        assert_eq!(target_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn reqwest_webhook_transport_caps_response_body_to_4kib() {
        let big_body = "x".repeat(16 * 1024);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            big_body.len(),
            big_body
        );
        let (url, _) = spawn_raw_http_server(resp).await;

        let transport = ReqwestWebhookTransport::new().with_allow_private_networks(true);
        let out = transport
            .send(&url, "t=1,v1=abc", "evt", "del-1", b"{}".to_vec())
            .await
            .expect("webhook delivery should succeed");
        assert_eq!(out.status, 200);
        let body = out.body.expect("body snippet present");
        assert_eq!(body.len(), MAX_WEBHOOK_RESPONSE_BYTES);
    }

    #[derive(Debug, Default)]
    struct CapturingTransport {
        urls: Mutex<Vec<String>>,
    }

    impl WebhookTransport for CapturingTransport {
        fn send<'a>(
            &'a self,
            url: &'a str,
            _signature_header: &'a str,
            _event_type: &'a str,
            _delivery_id: &'a str,
            _body: Vec<u8>,
        ) -> BoxFuture<'a, Result<WebhookHttpResponse, WebhookError>> {
            self.urls.lock().unwrap().push(url.to_string());
            Box::pin(async {
                Ok(WebhookHttpResponse {
                    status: 200,
                    body: Some("ok".into()),
                })
            })
        }
    }

    #[tokio::test]
    async fn webhook_dispatcher_isolates_deliveries_by_tenant() {
        let store = Arc::new(InMemoryWebhookDeliveryStore::new());
        let transport = Arc::new(CapturingTransport::default());
        let dispatcher = WebhookDispatcher::new(store, transport.clone());

        dispatcher.register_endpoint(
            WebhookEndpoint::new(
                "ep-a",
                "https://tenant-a.example/hook",
                SecretString::from("secret-a".to_string()),
            )
            .with_tenant_id("tenant-a"),
        );
        dispatcher.register_endpoint(
            WebhookEndpoint::new(
                "ep-b",
                "https://tenant-b.example/hook",
                SecretString::from("secret-b".to_string()),
            )
            .with_tenant_id("tenant-b"),
        );
        dispatcher.register_endpoint(WebhookEndpoint::new(
            "ep-global",
            "https://global.example/hook",
            SecretString::from("secret-g".to_string()),
        ));

        let deliveries_a = dispatcher
            .dispatch_event(
                Some("tenant-a"),
                "invoice.paid",
                serde_json::json!({"id": "inv-1"}),
            )
            .await
            .expect("dispatch tenant-a");
        assert_eq!(deliveries_a.len(), 1);
        assert_eq!(deliveries_a[0].endpoint_id, "ep-a");
        assert_eq!(
            *transport.urls.lock().unwrap(),
            vec!["https://tenant-a.example/hook".to_string()]
        );

        let deliveries_global = dispatcher
            .dispatch_event(None, "invoice.paid", serde_json::json!({"id": "inv-2"}))
            .await
            .expect("dispatch global");
        assert_eq!(deliveries_global.len(), 1);
        assert_eq!(deliveries_global[0].endpoint_id, "ep-global");
    }

    #[tokio::test]
    async fn concurrent_process_due_deliveries_sends_each_delivery_once() {
        use std::collections::HashMap;
        use tokio::sync::Barrier;

        #[derive(Debug, Default)]
        struct SlowCountingTransport {
            counts: Mutex<HashMap<String, usize>>,
        }

        impl WebhookTransport for SlowCountingTransport {
            fn send<'a>(
                &'a self,
                _url: &'a str,
                _signature_header: &'a str,
                _event_type: &'a str,
                delivery_id: &'a str,
                _body: Vec<u8>,
            ) -> BoxFuture<'a, Result<WebhookHttpResponse, WebhookError>> {
                let id = delivery_id.to_string();
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    *self.counts.lock().unwrap().entry(id).or_insert(0) += 1;
                    Ok(WebhookHttpResponse {
                        status: 200,
                        body: Some("ok".into()),
                    })
                })
            }
        }

        let store = Arc::new(InMemoryWebhookDeliveryStore::new());
        let transport = Arc::new(SlowCountingTransport::default());
        let dispatcher = Arc::new(WebhookDispatcher::new(store.clone(), transport.clone()));
        dispatcher.register_endpoint(WebhookEndpoint::new(
            "ep-concurrent",
            "https://example.test/hook",
            SecretString::from("secret".to_string()),
        ));

        let now = Utc::now();
        for i in 0..6 {
            store
                .upsert(WebhookDeliveryRecord {
                    id: format!("del-{i}"),
                    endpoint_id: "ep-concurrent".into(),
                    event_type: "order.paid".into(),
                    payload: format!(r#"{{"i":{i}}}"#),
                    status: WebhookDeliveryStatus::Pending,
                    attempt: 0,
                    next_attempt_at: Some(now - chrono::Duration::seconds(5)),
                    last_response: None,
                    created_at: now + chrono::Duration::milliseconds(i),
                    updated_at: now,
                })
                .await
                .expect("seed delivery");
        }

        let barrier = Arc::new(Barrier::new(4));
        let mut handles = Vec::with_capacity(4);
        for _ in 0..4 {
            let dispatcher = Arc::clone(&dispatcher);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                dispatcher
                    .process_due_deliveries(10)
                    .await
                    .expect("process_due_deliveries")
            }));
        }

        let mut total_attempted = 0usize;
        for h in handles {
            total_attempted += h.await.expect("task must not panic");
        }
        assert_eq!(total_attempted, 6);

        let counts = transport.counts.lock().unwrap();
        assert_eq!(counts.len(), 6);
        for i in 0..6 {
            let id = format!("del-{i}");
            assert_eq!(
                counts.get(&id).copied(),
                Some(1),
                "delivery {id} must be sent exactly once across 4 concurrent pollers"
            );
        }
    }
}
