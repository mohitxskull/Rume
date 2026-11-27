//! End-to-end integration tests for Milestone A: HTTP & Observability Baseline.
//!
//! Exercises transport middleware (body limits, CORS, compression), Prometheus/OpenMetrics
//! `/metrics` exposition, PII redaction, `#[derive(HasOpenApiSchema)]`, and OpenAPI Docs UI
//! wired through `Rume::new()` and the `rume` facade.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes, to_bytes};
use axum::http::{Method, Request, Response, StatusCode, header};
use garde::Validate;
use rume::metrics::{MetricsLayer, MetricsRegistry};
use rume::openapi::{OpenApiDocsConfig, OpenApiInfo, OpenApiUi};
use rume::prelude::*;
use rume::prometheus::{OPENMETRICS_CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE};
use rume::redaction::RedactionConfig;
use rume::transport::{CompressionConfig, CorsConfig, DEFAULT_BODY_LIMIT_BYTES};
use tower::{Layer, ServiceExt};
use tower_http::decompression::DecompressionLayer;
use tracing_subscriber::layer::SubscriberExt;

async fn decompress_response(response: Response<Body>, accept_encoding: &str) -> Vec<u8> {
    let mut response = Some(response);
    let svc = DecompressionLayer::new().layer(tower::service_fn(move |_req: Request<Body>| {
        let resp = response.take().expect("response already consumed");
        async move { Ok::<_, Infallible>(resp) }
    }));

    let decompressed = svc
        .oneshot(
            Request::builder()
                .header(header::ACCEPT_ENCODING, accept_encoding)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    to_bytes(Body::new(decompressed.into_body()), 1024 * 1024)
        .await
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn e2e_default_body_limit_rejects_over_2mib_and_respects_override_and_disable() {
    let over_2mib = DEFAULT_BODY_LIMIT_BYTES + 1;

    // 1. Default Rume::new() applies a 2 MiB body limit -> 2 MiB + 1 fails with 413.
    let default_app = Rume::new()
        .post(
            "/upload",
            |body: Bytes| async move { format!("{}", body.len()) },
        )
        .test_app()
        .unwrap();

    let resp = default_app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/upload")
                .header(header::CONTENT_LENGTH, over_2mib.to_string())
                .body(Body::from(vec![b'a'; over_2mib]))
                .unwrap(),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // 2. .body_limit(1024) accepts 1024 bytes and rejects 1025 bytes with 413.
    let small_limit_app = Rume::new()
        .body_limit(1024)
        .post(
            "/upload",
            |body: Bytes| async move { format!("{}", body.len()) },
        )
        .test_app()
        .unwrap();

    let ok_resp = small_limit_app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/upload")
                .header(header::CONTENT_LENGTH, "1024")
                .body(Body::from(vec![b'b'; 1024]))
                .unwrap(),
        )
        .await;
    assert_eq!(ok_resp.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(ok_resp).await, "1024");

    let too_large_resp = small_limit_app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/upload")
                .header(header::CONTENT_LENGTH, "1025")
                .body(Body::from(vec![b'b'; 1025]))
                .unwrap(),
        )
        .await;
    assert_eq!(too_large_resp.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // 3. .body_limit(3 MiB) accepts 2 MiB + 1 on a Bytes extractor handler
    //    (proving Axum's extractor-level DefaultBodyLimit::disable() is active).
    let large_limit_app = Rume::new()
        .body_limit(3 * 1024 * 1024)
        .post(
            "/upload",
            |body: Bytes| async move { format!("{}", body.len()) },
        )
        .test_app()
        .unwrap();

    let large_ok = large_limit_app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/upload")
                .header(header::CONTENT_LENGTH, over_2mib.to_string())
                .body(Body::from(vec![b'c'; over_2mib]))
                .unwrap(),
        )
        .await;
    assert_eq!(large_ok.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(large_ok).await, over_2mib.to_string());

    // 4. .without_body_limit() accepts payloads over 2 MiB.
    let unlimited_app = Rume::new()
        .without_body_limit()
        .post("/upload", |body: String| async move {
            format!("{}", body.len())
        })
        .test_app()
        .unwrap();

    let unlimited_ok = unlimited_app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/upload")
                .header(header::CONTENT_LENGTH, over_2mib.to_string())
                .body(Body::from(vec![b'd'; over_2mib]))
                .unwrap(),
        )
        .await;
    assert_eq!(unlimited_ok.status(), StatusCode::OK);
    assert_eq!(
        TestApp::body_text(unlimited_ok).await,
        over_2mib.to_string()
    );
}

#[tokio::test]
async fn e2e_cors_and_compression_preserve_problem_details_request_id() {
    let cors = CorsConfig::new()
        .allow_origin("https://app.example.com")
        .allow_methods(["GET", "POST"]);
    let compression = CompressionConfig::default().with_min_size_bytes(0);

    let app = Rume::new()
        .cors(cors)
        .compression(compression)
        .get("/fail", || async {
            Err::<&'static str, AppError>(AppError::BadRequest {
                code: "INVALID_PAYLOAD",
                message: "something went wrong while processing request".to_string(),
            })
        })
        .test_app()
        .unwrap();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/fail")
        .header(header::ORIGIN, "https://app.example.com")
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .unwrap();

    let resp = app.request(req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("https://app.example.com")
    );
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("gzip")
    );

    let header_req_id = resp
        .headers()
        .get(&rume::X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .expect("X-Request-ID header must be present")
        .to_string();

    let raw_bytes = decompress_response(resp, "gzip").await;
    let problem: serde_json::Value =
        serde_json::from_slice(&raw_bytes).expect("decompressed body must be valid JSON");

    assert_eq!(problem["status"], 400);
    assert_eq!(problem["code"], "INVALID_PAYLOAD");
    assert_eq!(
        problem["request_id"].as_str(),
        Some(header_req_id.as_str()),
        "ObservabilityLayer must inject request_id before CompressionLayer compresses the body"
    );
}

#[test]
fn e2e_invalid_cors_config_fails_closed_at_build_router() {
    let invalid_cors = CorsConfig::new().allow_any_origin().allow_credentials(true);

    let ctx = AppContext::new(
        (),
        (),
        ProjectPaths::from_current_dir(),
        Availability::new(),
        HealthChecks::new(),
        (),
    );

    let err = Rume::new()
        .cors(invalid_cors)
        .get("/ping", || async { "pong" })
        .build_router(ctx)
        .unwrap_err();

    assert!(
        matches!(
            err,
            BuildError::Cors(rume::transport::CorsConfigError::WildcardWithCredentials(_))
        ),
        "expected BuildError::Cors(WildcardWithCredentials), got: {err:?}"
    );
}

#[tokio::test]
async fn e2e_metrics_endpoint_exposes_prometheus_and_openmetrics_from_live_requests() {
    let registry = MetricsRegistry::new();

    let app = Rume::new()
        .get("/hello", || async { "world" })
        .metrics("/metrics", registry.clone())
        .layer(MetricsLayer::new(registry.clone()))
        .test_app()
        .unwrap();

    // Send two live requests to /hello.
    let r1 = app.get("/hello").await;
    assert_eq!(r1.status(), StatusCode::OK);
    let r2 = app.get("/hello").await;
    assert_eq!(r2.status(), StatusCode::OK);

    // 1. Default Prometheus 0.0.4 scrape.
    let prom_resp = app.get("/metrics").await;
    assert_eq!(prom_resp.status(), StatusCode::OK);
    assert_eq!(
        prom_resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some(PROMETHEUS_CONTENT_TYPE)
    );
    let prom_body = TestApp::body_text(prom_resp).await;
    assert!(
        prom_body.contains("http_requests_total"),
        "expected http_requests_total in Prometheus output:\n{prom_body}"
    );
    assert!(
        prom_body.contains("http_request_duration_seconds_bucket"),
        "expected http_request_duration_seconds_bucket in Prometheus output:\n{prom_body}"
    );

    // 2. OpenMetrics 1.0 content-negotiated scrape.
    let om_resp = app
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/metrics")
                .header(
                    header::ACCEPT,
                    "application/openmetrics-text; version=1.0.0",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(om_resp.status(), StatusCode::OK);
    assert_eq!(
        om_resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some(OPENMETRICS_CONTENT_TYPE)
    );
    let om_body = TestApp::body_text(om_resp).await;
    assert!(
        om_body.ends_with("# EOF\n"),
        "expected OpenMetrics output to end with # EOF\\n:\n{om_body}"
    );
    assert!(om_body.contains("http_requests_total"));
    assert!(om_body.contains("http_request_duration_seconds_bucket"));

    // 3. Verify /metrics scrapes themselves were bypassed by MetricsLayer (still exactly 2 GET 200 requests).
    let snap = registry.snapshot();
    assert_eq!(
        snap.counter_u64(
            "http_requests_total",
            &[("method", "GET"), ("status", "200")]
        ),
        Some(2),
        "/metrics scrapes must be bypassed by MetricsLayer"
    );
}

#[derive(Debug, Deserialize, Validate, rume::HasOpenApiSchema)]
#[serde(rename_all = "camelCase")]
struct CreateUserPayload {
    #[garde(email)]
    email_address: String,
    #[garde(length(min = 3, max = 50))]
    display_name: String,
    #[garde(range(min = 18, max = 120))]
    age: u8,
    #[garde(skip)]
    bio_note: Option<String>,
}

#[tokio::test]
async fn e2e_derive_has_openapi_schema_serves_in_openapi_json_and_docs_ui() {
    let info = OpenApiInfo::new("Users API", "1.0.0");
    let docs_cfg = OpenApiDocsConfig::new("/openapi.json")
        .with_title("Users API Docs")
        .with_ui(OpenApiUi::Scalar);

    let app = Rume::new()
        .route_spec(
            RouteSpec::post("/users").with_body::<ValidatedJson<CreateUserPayload>>(),
            |ValidatedJson(payload): ValidatedJson<CreateUserPayload>| async move {
                format!(
                    "{}:{}:{}:{}",
                    payload.email_address,
                    payload.display_name,
                    payload.age,
                    payload.bio_note.unwrap_or_default()
                )
            },
        )
        .openapi_docs("/docs", "/openapi.json", info, docs_cfg)
        .test_app()
        .unwrap();

    // 1. Inspect GET /openapi.json.
    let spec_resp = app.get("/openapi.json").await;
    assert_eq!(spec_resp.status(), StatusCode::OK);
    let spec_json: serde_json::Value =
        serde_json::from_str(&TestApp::body_text(spec_resp).await).unwrap();

    let schema = &spec_json["paths"]["/users"]["post"]["requestBody"]["content"]["application/json"]
        ["schema"];
    assert_eq!(schema["type"], "object");

    let props = schema["properties"]
        .as_object()
        .expect("schema.properties must be an object");
    assert_eq!(props["emailAddress"]["type"], "string");
    assert_eq!(props["emailAddress"]["format"], "email");
    assert_eq!(props["displayName"]["type"], "string");
    assert_eq!(props["displayName"]["minLength"], 3);
    assert_eq!(props["displayName"]["maxLength"], 50);
    assert_eq!(props["age"]["type"], "integer");
    assert_eq!(props["age"]["minimum"], 18);
    assert_eq!(props["age"]["maximum"], 120);
    assert_eq!(props["bioNote"]["type"], "string");

    let required: Vec<&str> = schema["required"]
        .as_array()
        .expect("schema.required must be an array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(required.contains(&"emailAddress"));
    assert!(required.contains(&"displayName"));
    assert!(required.contains(&"age"));
    assert!(
        !required.contains(&"bioNote"),
        "optional field bioNote must not be in required list: {required:?}"
    );

    // 2. Inspect GET /docs.
    let docs_resp = app.get("/docs").await;
    assert_eq!(docs_resp.status(), StatusCode::OK);
    assert_eq!(
        docs_resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    let html = TestApp::body_text(docs_resp).await;
    assert!(html.contains("<title>Users API Docs</title>"));
    assert!(html.contains(r#"data-url="/openapi.json""#));
    assert!(html.contains("@scalar/api-reference"));
}

#[tokio::test]
async fn e2e_redaction_config_masks_headers_and_json_body_without_breaking_streaming() {
    #[derive(Default)]
    struct FieldCollector {
        output: String,
    }

    impl tracing::field::Visit for FieldCollector {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write as _;
            let _ = write!(self.output, "{}={:?} ", field.name(), value);
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            use std::fmt::Write as _;
            let _ = write!(self.output, "{}={} ", field.name(), value);
        }
    }

    struct EventCaptureLayer {
        events: Arc<Mutex<Vec<String>>>,
    }

    impl<S> tracing_subscriber::Layer<S> for EventCaptureLayer
    where
        S: tracing::Subscriber,
    {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = FieldCollector::default();
            event.record(&mut visitor);
            self.events.lock().unwrap().push(visitor.output);
        }
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(EventCaptureLayer {
        events: Arc::clone(&captured),
    });
    let _sub_guard = tracing::subscriber::set_default(subscriber);
    tracing::callsite::rebuild_interest_cache();

    let redaction = RedactionConfig::new()
        .with_log_headers(true)
        .with_sensitive_header("x-custom-secret");

    // 1. Verify RedactionConfig::redact_json_bytes masks PII keys.
    let raw_json = br#"{"user":"alice","password":"hunter2","nested":{"api_key":"sk-live-123"}}"#;
    let redacted_json = redaction
        .redact_json_bytes(raw_json)
        .expect("valid bounded JSON must redact");
    assert!(redacted_json.contains("\"user\":\"alice\""));
    assert!(redacted_json.contains("[REDACTED]"));
    assert!(!redacted_json.contains("hunter2"));
    assert!(!redacted_json.contains("sk-live-123"));

    // 2. Wire .redaction(...) into Rume::new() with a streaming endpoint.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    tx.send(Ok(Bytes::from_static(b"chunk-1-"))).await.unwrap();
    let rx_holder = Arc::new(Mutex::new(Some(rx)));

    let app = Rume::new()
        .redaction(redaction)
        .get("/stream", move || {
            let rx_holder = Arc::clone(&rx_holder);
            async move {
                let mut rx = rx_holder.lock().unwrap().take().unwrap();
                let stream = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));
                Response::builder()
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }
        })
        .test_app()
        .unwrap();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/stream")
        .header(header::AUTHORIZATION, "Bearer super-secret-jwt")
        .header("x-custom-secret", "custom-secret-val")
        .header("x-public-header", "visible-value")
        .body(Body::empty())
        .unwrap();

    // Streaming response must resolve before the stream terminates (proving no body buffering).
    let resp = tokio::time::timeout(Duration::from_secs(2), app.request(req))
        .await
        .expect("streaming response must not block waiting for full body");
    assert_eq!(resp.status(), StatusCode::OK);

    tx.send(Ok(Bytes::from_static(b"chunk-2"))).await.unwrap();
    drop(tx);

    let streamed_bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
    assert_eq!(streamed_bytes.as_ref(), b"chunk-1-chunk-2");

    // 3. Verify sensitive headers were redacted in captured logs while non-sensitive ones survived.
    let all_logged = captured.lock().unwrap().join("\n");
    assert!(
        all_logged.contains("[REDACTED]"),
        "expected [REDACTED] in captured log events: {all_logged}"
    );
    assert!(
        !all_logged.contains("super-secret-jwt"),
        "raw Authorization header leaked into logs: {all_logged}"
    );
    assert!(
        !all_logged.contains("custom-secret-val"),
        "raw custom sensitive header leaked into logs: {all_logged}"
    );
    assert!(
        all_logged.contains("visible-value"),
        "non-sensitive header should be preserved in logs: {all_logged}"
    );
}
