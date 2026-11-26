//! Milestone E end-to-end integration tests through the `rume` facade.
//!
//! Exercises:
//! 1. Native TLS (`TlsConfig::from_pem_bytes` + `RumeBuilder::tls`), `RealtimeHub` (WebSocket +
//!    SSE tenant-scoped broadcast + graceful close/reconnect drain), static file serving + SPA
//!    fallback, `HttpCacheLayer` (`ETag` / `If-None-Match` -> `304 Not Modified`), `/v1` API
//!    versioning + RFC 8594 `Sunset`/`Deprecation` headers, and clean server shutdown drain.
//! 2. `CachedSecretProvider` TTL/rotation, `SqlFeatureFlags` (SQLite) targeting + SHA-256 rollout,
//!    `SqlUsageMeter` (SQLite) + `BillingSink` flush, outbound/inbound HMAC-SHA256 webhooks,
//!    `TestMailer`, SCIM 2.0 `/scim/v2/Users` & `/scim/v2/Groups` CRUD + RFC 7644 filters, and
//!    `rume-grpc` + `rume-graphql` integration.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_graphql::{EmptyMutation, EmptySubscription, Object, Schema};
use axum::Extension;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::routing::post;
use rume::cache::InMemoryCacheBackend;
use rume::enterprise::scim::{ScimGroup, ScimListResponse, ScimUser};
use rume::env::{ExternalSecretBackend, SecretProviderError, VaultSecretProvider};
use rume::flags::{TargetingRule, evaluate_flag};
use rume::graphql::{GraphQlConfig, RumeTraceExtension, app_error_to_graphql, graphql_routes};
use rume::grpc::{RumeTraceInterceptor, app_error_to_status};
use rume::metering::{MeteringError, UsageRecord};
use rume::persistence::SqliteBackend;
use rume::prelude::*;
use rume::realtime::{RealtimeTicketSigner, sse_handler, ws_handler};
use rume::routing::versioned_group;
use rume::webhooks::{
    InMemoryWebhookDeliveryStore, StaticWebhookSecretResolver, WebhookDeliveryStatus, WebhookError,
    WebhookHttpResponse, WebhookTransport,
};
use secrecy::{ExposeSecret, SecretString};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tonic::service::Interceptor as _;
use tower::ServiceExt;

const TEST_CERT_PEM: &[u8] = b"\
-----BEGIN CERTIFICATE-----
MIIBwDCCAWWgAwIBAgIUIi3+ZI/tJFK4wTVlTFPD6QkG8XAwCgYIKoZIzj0EAwIw
FDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MTEyNjEyMTYwMloYDzIxMDAxMDI5
MTIxNjAyWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjO
PQMBBwNCAATIw5glQvkynffHCd5iEclrqe+juD6xFx8A1F43+PqLjpra4wZ3non1
Dp6cLKCLOtYHLB05Rf2+7jxnMmyDVrryo4GSMIGPMB0GA1UdDgQWBBQQ8rRAB+Bs
+SsH4WcR+cJEbH+hTzAfBgNVHSMEGDAWgBQQ8rRAB+Bs+SsH4WcR+cJEbH+hTzAM
BgNVHRMBAf8EAjAAMA4GA1UdDwEB/wQEAwIFoDATBgNVHSUEDDAKBggrBgEFBQcD
ATAaBgNVHREEEzARgglsb2NhbGhvc3SHBH8AAAEwCgYIKoZIzj0EAwIDSQAwRgIh
APyYGMmbXQV05SjOkoWARBTijDUQ9v3rvdTvfFFJ8HXSAiEA/rJYP8/AcQalmlJL
MOAtrICgSzS3UyLAhl16728zqGc=
-----END CERTIFICATE-----
";

const TEST_KEY_PEM: &[u8] = b"\
-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgo2otzH6WgCrTvKb8
Mad8NskKE+yg2Tqk/S0mX41X1AGhRANCAATIw5glQvkynffHCd5iEclrqe+juD6x
Fx8A1F43+PqLjpra4wZ3non1Dp6cLKCLOtYHLB05Rf2+7jxnMmyDVrry
-----END PRIVATE KEY-----
";

fn tls_connector_for_test_cert() -> tokio_rustls::TlsConnector {
    let mut root_store = rustls::RootCertStore::empty();
    let mut cert_reader = std::io::BufReader::new(TEST_CERT_PEM);
    for cert in rustls_pemfile::certs(&mut cert_reader) {
        root_store.add(cert.expect("valid test cert")).unwrap();
    }
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
    let client_cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(client_cfg))
}

async fn send_tls_http_request(
    connector: &tokio_rustls::TlsConnector,
    addr: std::net::SocketAddr,
    raw_request: &str,
) -> String {
    let server_name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
    let tcp = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect tcp");
    let mut tls_stream = connector
        .connect(server_name, tcp)
        .await
        .expect("tls handshake");
    tls_stream
        .write_all(raw_request.as_bytes())
        .await
        .expect("write tls request");
    let mut response = String::new();
    tls_stream
        .read_to_string(&mut response)
        .await
        .expect("read tls response");
    response
}

fn extract_header(raw_http: &str, name: &str) -> Option<String> {
    let target = name.to_ascii_lowercase();
    let head = raw_http.split("\r\n\r\n").next()?;
    for line in head.lines().skip(1) {
        if let Some((k, v)) = line.split_once(':')
            && k.trim().to_ascii_lowercase() == target
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

async fn wait_for_ready(
    lifecycle: &Lifecycle,
    server: &mut tokio::task::JoinHandle<Result<(), std::io::Error>>,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if server.is_finished() {
            let res = server.await.expect("server task panicked");
            panic!("server exited prematurely: {res:?}");
        }
        if lifecycle.state() == LifecycleState::Ready {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "server did not reach Ready within {timeout:?}, state={:?}",
        lifecycle.state()
    );
}

#[tokio::test]
async fn milestone_e_tls_realtime_caching_versioning_e2e() {
    // Prepare static directory with index.html (SPA fallback) and app.js
    let static_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(static_dir.path().join("index.html"), "<html>SPA</html>").unwrap();
    std::fs::write(static_dir.path().join("app.js"), "console.log('rume');").unwrap();

    let tls_config =
        TlsConfig::from_pem_bytes(TEST_CERT_PEM, TEST_KEY_PEM).expect("valid PEM cert and key");

    let hub = RealtimeHub::new(RealtimeConfig::default());
    let ticket_signer =
        RealtimeTicketSigner::new(b"0123456789abcdef0123456789abcdef").expect("ticket signer");

    let cache = Arc::new(InMemoryCacheBackend::new());
    let v1_router = versioned_group::<AppContext<(), (), ()>>(ApiVersion::V1)
        .get("/items", || async { "v1-items" })
        .layer(
            DeprecationLayer::new()
                .deprecation_timestamp(1_700_000_000)
                .sunset("Wed, 31 Dec 2026 23:59:59 GMT"),
        )
        .build();

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_hook_ran = Arc::new(AtomicBool::new(false));
    let hook_flag = Arc::clone(&shutdown_hook_ran);

    let app = Rume::new()
        .router(v1_router)
        .tls(tls_config)
        .realtime(hub.clone())
        .get("/cached", || async { "cacheable-payload" })
        .http_cache(HttpCacheLayer::new(cache, HttpCacheConfig::default()))
        .get("/ws/{room}", ws_handler)
        .get("/sse/{room}", sse_handler)
        .static_files(
            StaticFilesConfig::new(static_dir.path())
                .with_spa_fallback(static_dir.path().join("index.html")),
        )
        .layer(Extension(ticket_signer.clone()))
        .on_shutdown(move |ctx| async move {
            assert_eq!(ctx.lifecycle.state(), LifecycleState::Terminating);
            assert_eq!(ctx.availability.state(), AvailabilityState::RefusingTraffic);
            hook_flag.store(true, Ordering::SeqCst);
        });

    let lifecycle = app.lifecycle();
    let availability = app.availability();

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let mut server = tokio::spawn(async move {
        app.run_with_listener_and_signal(listener, async move {
            let _ = shutdown_rx.await;
        })
        .await
    });

    wait_for_ready(&lifecycle, &mut server, Duration::from_secs(5)).await;
    let connector = tls_connector_for_test_cert();

    // 1. HTTPS cached endpoint -> 200 OK + ETag, then If-None-Match -> 304 Not Modified
    let first_cached = send_tls_http_request(
        &connector,
        addr,
        "GET /cached HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(
        first_cached.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK for /cached, got: {first_cached}"
    );
    assert!(first_cached.contains("cacheable-payload"));
    let etag = extract_header(&first_cached, "etag").expect("ETag header must be present");

    let second_cached = send_tls_http_request(
        &connector,
        addr,
        &format!(
            "GET /cached HTTP/1.1\r\nHost: localhost\r\nIf-None-Match: {etag}\r\nConnection: close\r\n\r\n"
        ),
    )
    .await;
    assert!(
        second_cached.starts_with("HTTP/1.1 304 Not Modified"),
        "expected 304 Not Modified, got: {second_cached}"
    );

    // 2. Static file serving + SPA fallback over TLS
    let js_resp = send_tls_http_request(
        &connector,
        addr,
        "GET /app.js HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(js_resp.starts_with("HTTP/1.1 200 OK"));
    assert!(js_resp.contains("console.log('rume');"));

    let spa_resp = send_tls_http_request(
        &connector,
        addr,
        "GET /dashboard/settings HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(spa_resp.starts_with("HTTP/1.1 200 OK"));
    assert!(spa_resp.contains("<html>SPA</html>"));

    // 3. /v1 deprecated route emits RFC 8594 Deprecation + Sunset headers
    let v1_resp = send_tls_http_request(
        &connector,
        addr,
        "GET /v1/items HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(v1_resp.starts_with("HTTP/1.1 200 OK"));
    assert!(v1_resp.contains("v1-items"));
    assert!(extract_header(&v1_resp, "deprecation").is_some());
    assert_eq!(
        extract_header(&v1_resp, "sunset").as_deref(),
        Some("Wed, 31 Dec 2026 23:59:59 GMT")
    );

    // 4. RealtimeHub SSE + WebSocket over TLS with tenant isolation & graceful shutdown drain
    let ticket = ticket_signer
        .issue("user-1", "tenant-a", Some("room-1".into()), 60)
        .expect("issue ticket");

    let sse_tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let server_name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
    let mut sse_tls = connector
        .connect(server_name.clone(), sse_tcp)
        .await
        .unwrap();
    let sse_req = format!(
        "GET /sse/room-1?ticket={ticket} HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n\r\n"
    );
    sse_tls.write_all(sse_req.as_bytes()).await.unwrap();

    let mut sse_reader = BufReader::new(sse_tls);
    let mut status_line = String::new();
    sse_reader.read_line(&mut status_line).await.unwrap();
    assert!(status_line.starts_with("HTTP/1.1 200 OK"));
    loop {
        let mut line = String::new();
        sse_reader.read_line(&mut line).await.unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
    }

    // Broadcast to tenant-b (must NOT reach tenant-a) and tenant-a (MUST reach tenant-a)
    hub.publish(
        "tenant-b",
        "room-1",
        RealtimeEvent::new("msg", serde_json::json!({"text": "wrong-tenant"})),
    )
    .await
    .expect("publish tenant-b");
    hub.publish(
        "tenant-a",
        "room-1",
        RealtimeEvent::new("msg", serde_json::json!({"text": "hello-tenant-a"})),
    )
    .await
    .expect("publish tenant-a");

    let mut sse_body = String::new();
    let found = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let mut line = String::new();
            let n = sse_reader.read_line(&mut line).await.unwrap();
            if n == 0 {
                break false;
            }
            sse_body.push_str(&line);
            if sse_body.contains("hello-tenant-a") {
                break true;
            }
        }
    })
    .await
    .expect("SSE event timeout");
    assert!(found, "expected tenant-a SSE event, got: {sse_body}");
    assert!(!sse_body.contains("wrong-tenant"));

    // Also verify WebSocket handshake over TLS
    let ws_tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut ws_tls = connector.connect(server_name, ws_tcp).await.unwrap();
    let ws_req = format!(
        "GET /ws/room-1?ticket={ticket} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    ws_tls.write_all(ws_req.as_bytes()).await.unwrap();
    let mut ws_reader = BufReader::new(ws_tls);
    let mut ws_status = String::new();
    ws_reader.read_line(&mut ws_status).await.unwrap();
    assert!(
        ws_status.starts_with("HTTP/1.1 101"),
        "expected 101 Switching Protocols, got: {ws_status}"
    );

    // Trigger server graceful shutdown (also drains RealtimeHub)
    let _ = shutdown_tx.send(());
    let server_res = server.await.expect("server task panicked");
    assert!(
        server_res.is_ok(),
        "server exited with error: {server_res:?}"
    );
    assert_eq!(lifecycle.state(), LifecycleState::Terminated);
    assert_eq!(availability.state(), AvailabilityState::Unknown);
    assert!(shutdown_hook_ran.load(Ordering::SeqCst));
    assert!(hub.is_shutting_down());
}

#[derive(Debug)]
struct MockSecretBackend {
    secrets: std::sync::RwLock<HashMap<String, String>>,
}

impl ExternalSecretBackend for MockSecretBackend {
    fn name(&self) -> &str {
        "mock-vault"
    }

    fn fetch_secret<'a>(
        &'a self,
        key: &'a str,
    ) -> rume::env::secret_provider::BoxFuture<'a, Result<SecretString, SecretProviderError>> {
        let key = key.to_string();
        Box::pin(async move {
            self.secrets
                .read()
                .unwrap()
                .get(&key)
                .cloned()
                .map(SecretString::from)
                .ok_or(SecretProviderError::NotFound { key })
        })
    }

    fn rotate_secret<'a>(
        &'a self,
        key: &'a str,
    ) -> rume::env::secret_provider::BoxFuture<'a, Result<SecretString, SecretProviderError>> {
        let key = key.to_string();
        Box::pin(async move {
            let mut guard = self.secrets.write().unwrap();
            let next = format!(
                "{}-v2",
                guard.get(&key).cloned().unwrap_or_else(|| "init".into())
            );
            guard.insert(key, next.clone());
            Ok(SecretString::from(next))
        })
    }
}

#[derive(Default)]
struct CapturingBillingSink {
    batches: Mutex<Vec<Vec<UsageRecord>>>,
}

impl BillingSink for CapturingBillingSink {
    fn export_batch<'a>(
        &'a self,
        records: &'a [UsageRecord],
    ) -> rume::metering::BoxFuture<'a, Result<(), MeteringError>> {
        Box::pin(async move {
            self.batches.lock().unwrap().push(records.to_vec());
            Ok(())
        })
    }
}

#[derive(Debug)]
struct InProcessWebhookTransport {
    router: axum::Router,
}

impl WebhookTransport for InProcessWebhookTransport {
    fn send<'a>(
        &'a self,
        url: &'a str,
        signature_header: &'a str,
        event_type: &'a str,
        delivery_id: &'a str,
        body: Vec<u8>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<WebhookHttpResponse, WebhookError>> + Send + 'a,
        >,
    > {
        let router = self.router.clone();
        let path = url
            .strip_prefix("http://in-process")
            .unwrap_or(url)
            .to_string();
        let sig = signature_header.to_string();
        let ev = event_type.to_string();
        let deliv = delivery_id.to_string();
        Box::pin(async move {
            let req = Request::builder()
                .method("POST")
                .uri(path)
                .header("Content-Type", "application/json")
                .header("X-Rume-Signature", sig)
                .header("X-Rume-Event", ev)
                .header("X-Rume-Delivery", deliv)
                .body(Body::from(body))
                .map_err(|e| WebhookError::Transport(e.to_string()))?;
            let resp = router
                .oneshot(req)
                .await
                .map_err(|e| WebhookError::Transport(e.to_string()))?;
            let status = resp.status().as_u16();
            let bytes = to_bytes(resp.into_body(), usize::MAX)
                .await
                .map_err(|e| WebhookError::Transport(e.to_string()))?;
            Ok(WebhookHttpResponse {
                status,
                body: Some(String::from_utf8_lossy(&bytes).into_owned()),
            })
        })
    }
}

struct GraphQlQueryRoot;

#[Object]
impl GraphQlQueryRoot {
    async fn ping(&self) -> &str {
        "pong"
    }

    async fn missing(&self) -> async_graphql::Result<&str> {
        Err(app_error_to_graphql(&AppError::not_found("missing item")))
    }
}

#[tokio::test]
async fn milestone_e_platform_scim_and_rpc_e2e() {
    // 1. CachedSecretProvider TTL + rotation callback
    let mut initial = HashMap::new();
    initial.insert("secret/API_KEY".to_string(), "secret-v1".to_string());
    let backend_secrets = Arc::new(MockSecretBackend {
        secrets: std::sync::RwLock::new(initial),
    });
    let rotated_keys = Arc::new(Mutex::new(Vec::<String>::new()));
    let rk = Arc::clone(&rotated_keys);
    let secret_provider = CachedSecretProvider::new(
        Arc::new(VaultSecretProvider::new(backend_secrets, "secret")),
        Duration::from_secs(60),
    )
    .with_on_rotation(move |key| {
        rk.lock().unwrap().push(key.to_string());
    });
    let v1 = secret_provider.get_secret("API_KEY").await.unwrap();
    assert_eq!(v1.expose_secret(), "secret-v1");
    let v2 = secret_provider.rotate("API_KEY").await.unwrap();
    assert_eq!(v2.expose_secret(), "secret-v1-v2");
    assert_eq!(*rotated_keys.lock().unwrap(), vec!["API_KEY".to_string()]);

    // 2. SqlFeatureFlags (SQLite) + deterministic SHA-256 rollout
    let sql_backend = Arc::new(SqliteBackend::in_memory().await.expect("sqlite backend"));
    let flag_store = SqlFeatureFlags::new(Arc::clone(&sql_backend));
    flag_store.ensure_schema().await.expect("init flags schema");

    let flag = FeatureFlag::new("new_checkout", true)
        .with_rollout(50)
        .with_rule(TargetingRule::Tenant("vip-tenant".into()));
    flag_store.upsert_flag(flag).await.expect("upsert flag");
    let loaded_flag = flag_store
        .get_flag("new_checkout", None)
        .await
        .expect("get flag")
        .expect("flag exists");
    assert!(evaluate_flag(
        &loaded_flag,
        &EvaluationContext::new().with_tenant("vip-tenant")
    ));
    let ctx = EvaluationContext::new().with_user("user-42");
    assert_eq!(
        evaluate_flag(&loaded_flag, &ctx),
        evaluate_flag(&loaded_flag, &ctx)
    );

    // 3. SqlUsageMeter (SQLite) + BillingSink flush
    let meter = SqlUsageMeter::new(Arc::clone(&sql_backend));
    meter.ensure_schema().await.expect("init metering schema");
    meter
        .record_usage("tenant-1", "api_requests", 12)
        .await
        .expect("record usage");
    assert_eq!(
        meter
            .get_usage("tenant-1", "api_requests")
            .await
            .expect("get usage"),
        12
    );
    let sink = CapturingBillingSink::default();
    let exported = meter.flush(&sink).await.expect("flush usage");
    assert_eq!(exported, 1);
    assert_eq!(sink.batches.lock().unwrap()[0][0].quantity, 12);

    // 4. Outbound WebhookDispatcher HMAC-SHA256 -> Inbound VerifyWebhookSignatureLayer
    let webhook_secret = SecretString::from("whsec_e2e_test_secret_key_123456".to_string());
    let received_webhooks = Arc::new(AtomicUsize::new(0));
    let rw = Arc::clone(&received_webhooks);
    let inbound_router = axum::Router::new()
        .route(
            "/hooks/inbound",
            post(move || {
                let rw = Arc::clone(&rw);
                async move {
                    rw.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        )
        .layer(VerifyWebhookSignatureLayer::new(
            WebhookSignatureConfig::new(Arc::new(StaticWebhookSecretResolver::new(
                webhook_secret.clone(),
            ))),
        ));

    let delivery_store = Arc::new(InMemoryWebhookDeliveryStore::new());
    let transport = Arc::new(InProcessWebhookTransport {
        router: inbound_router,
    });
    let dispatcher = WebhookDispatcher::new(delivery_store, transport);
    dispatcher.register_endpoint(WebhookEndpoint::new(
        "ep-1",
        "http://in-process/hooks/inbound",
        webhook_secret,
    ));
    let deliveries = dispatcher
        .dispatch_event(None, "invoice.paid", serde_json::json!({"id": "inv_1"}))
        .await
        .expect("dispatch webhook");
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].status, WebhookDeliveryStatus::Delivered);
    assert_eq!(received_webhooks.load(Ordering::SeqCst), 1);

    // 5. TestMailer email capture
    let mailer = TestMailer::new();
    mailer
        .send(
            EmailMessage::new(
                "noreply@rume.dev",
                vec!["alice@example.com".into()],
                "Welcome",
            )
            .with_text_body("Hello Alice"),
        )
        .await
        .expect("send email");
    assert_eq!(mailer.sent().len(), 1);
    assert_eq!(mailer.sent()[0].subject, "Welcome");

    // 6. SCIM 2.0 /scim/v2/Users & /scim/v2/Groups CRUD + RFC 7644 filter
    let scim_app = scim_routes(InMemoryScimStore::new());
    let create_user = scim_app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/scim/v2/Users")
                .header(header::CONTENT_TYPE, "application/scim+json")
                .body(Body::from(
                    serde_json::to_vec(&ScimUser::new("", "alice@example.com")).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_user.status(), StatusCode::CREATED);
    let created_user: ScimUser =
        serde_json::from_slice(&to_bytes(create_user.into_body(), usize::MAX).await.unwrap())
            .unwrap();

    let create_group = scim_app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/scim/v2/Groups")
                .header(header::CONTENT_TYPE, "application/scim+json")
                .body(Body::from(
                    serde_json::to_vec(&ScimGroup::new("", "Engineering")).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_group.status(), StatusCode::CREATED);

    let filter_users = scim_app
        .oneshot(
            Request::builder()
                .uri("/scim/v2/Users?filter=userName%20sw%20%22alice%22")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(filter_users.status(), StatusCode::OK);
    let listed: ScimListResponse<ScimUser> = serde_json::from_slice(
        &to_bytes(filter_users.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(listed.total_results, 1);
    assert_eq!(listed.resources[0].id, created_user.id);

    // 7. rume::grpc + rume::graphql integration
    assert_eq!(
        app_error_to_status(&AppError::NotFound).code(),
        tonic::Code::NotFound
    );
    let mut meta = tonic::metadata::MetadataMap::new();
    meta.insert(
        "traceparent",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
            .parse()
            .unwrap(),
    );
    let mut interceptor = RumeTraceInterceptor;
    let intercepted = interceptor
        .call(tonic::Request::from_parts(
            meta,
            tonic::Extensions::new(),
            (),
        ))
        .expect("grpc trace interceptor");
    assert!(
        intercepted
            .extensions()
            .get::<rume::app::TraceContext>()
            .is_some()
    );

    let schema = Schema::build(GraphQlQueryRoot, EmptyMutation, EmptySubscription)
        .extension(RumeTraceExtension)
        .finish();
    let gql_app = graphql_routes(schema, GraphQlConfig::default());
    let gql_res = gql_app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/graphql")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"query":"{ ping missing }"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(gql_res.status(), StatusCode::OK);
    let gql_json: serde_json::Value =
        serde_json::from_slice(&to_bytes(gql_res.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(gql_json["data"]["ping"], "pong");
    assert_eq!(gql_json["errors"][0]["extensions"]["code"], "NOT_FOUND");
}
