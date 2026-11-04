//! Test application builder and simulated HTTP client for Axum routes.

use std::sync::Mutex;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use tower::ServiceExt;

/// A lightweight test application backed by an Axum router.
pub struct TestApp {
    router: Router<()>,
    cleanup: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl TestApp {
    /// Create a test app from an already-built router.
    pub fn new(router: impl Into<Router<()>>) -> Self {
        Self {
            router: router.into(),
            cleanup: Mutex::new(None),
        }
    }

    /// Create a test app with a cleanup hook for DB-backed tests.
    pub fn with_cleanup(
        router: impl Into<Router<()>>,
        cleanup: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            router: router.into(),
            cleanup: Mutex::new(Some(Box::new(cleanup))),
        }
    }

    /// Builds a test app from a [`crate::app::builder::RumeBuilder`], executing module startup hooks and attaching lifecycle teardown.
    pub async fn from_builder<E, C, S>(
        builder: crate::app::builder::RumeBuilder<E, C, S>,
    ) -> Result<Self, crate::app::builder::BuildError>
    where
        E: Clone + Send + Sync + 'static,
        C: Clone + Send + Sync + 'static,
        S: Clone + Send + Sync + 'static,
    {
        builder.build_test_app().await
    }

    /// Consumes the test application and returns its underlying Axum [`Router`].
    pub fn into_router(self) -> Router<()> {
        self.router.clone()
    }

    /// Returns a reference to the underlying Axum [`Router`].
    pub fn router(&self) -> &Router<()> {
        &self.router
    }

    /// Issue a GET request.
    pub async fn get(&self, uri: &str) -> Response<Body> {
        self.request(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    /// Issue a GET request returning a fluent [`TestResponse`].
    pub async fn get_res(&self, uri: &str) -> TestResponse {
        TestResponse::from_response(self.get(uri).await).await
    }

    /// Issue a POST request with a JSON body.
    pub async fn post_json(&self, uri: &str, body: impl Into<String>) -> Response<Body> {
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.into()))
            .unwrap();
        self.request(req).await
    }

    /// Issue a POST request with JSON body returning a fluent [`TestResponse`].
    pub async fn post_res(&self, uri: &str, body: impl Into<String>) -> TestResponse {
        TestResponse::from_response(self.post_json(uri, body).await).await
    }

    /// Issue an arbitrary request.
    pub async fn request(&self, req: Request<Body>) -> Response<Body> {
        self.router
            .clone()
            .oneshot(req)
            .await
            .expect("request failed")
    }

    /// Issue an arbitrary request returning a fluent [`TestResponse`].
    pub async fn request_res(&self, req: Request<Body>) -> TestResponse {
        TestResponse::from_response(self.request(req).await).await
    }

    /// Read the response body as bytes.
    pub async fn body_bytes(response: Response<Body>) -> Vec<u8> {
        use axum::body::to_bytes;
        to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await
            .expect("failed to read body")
            .to_vec()
    }

    /// Read the response body as text.
    pub async fn body_text(response: Response<Body>) -> String {
        String::from_utf8(Self::body_bytes(response).await).expect("body is not utf-8")
    }

    /// Run teardown explicitly.
    pub fn teardown(self) {
        if let Ok(mut lock) = self.cleanup.lock()
            && let Some(cleanup) = lock.take()
        {
            cleanup();
        }
    }

    /// Run teardown asynchronously without blocking the active Tokio runtime worker thread.
    pub async fn teardown_async(self) {
        let _ = tokio::task::spawn_blocking(move || self.teardown()).await;
    }
}

impl std::fmt::Debug for TestApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let has_cleanup = self.cleanup.lock().map(|l| l.is_some()).unwrap_or(false);
        f.debug_struct("TestApp")
            .field("has_cleanup", &has_cleanup)
            .finish()
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        if let Ok(mut lock) = self.cleanup.lock()
            && let Some(cleanup) = lock.take()
        {
            cleanup();
        }
    }
}

/// Builder for test applications.
pub struct TestAppBuilder<S> {
    state: S,
    router: Router<S>,
    cleanup: Option<Box<dyn FnOnce() + Send>>,
}

impl<S> TestAppBuilder<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub fn new(state: S) -> Self {
        Self {
            state,
            router: Router::new(),
            cleanup: None,
        }
    }

    pub fn router(mut self, router: impl Into<Router<S>>) -> Self {
        self.router = router.into();
        self
    }

    pub fn route(mut self, path: &str, method_router: axum::routing::MethodRouter<S>) -> Self {
        self.router = self.router.route(path, method_router);
        self
    }

    /// Register a cleanup hook for DB-backed tests.
    pub fn with_cleanup(mut self, cleanup: impl FnOnce() + Send + 'static) -> Self {
        self.cleanup = Some(Box::new(cleanup));
        self
    }

    /// Enlists a transaction rollback hook to cleanly revert database state when the test app drops.
    pub fn with_transaction_rollback<F>(mut self, rollback_fn: F) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        self.cleanup = Some(Box::new(rollback_fn));
        self
    }

    pub async fn build(self) -> TestApp {
        let cleanup = self.cleanup;
        let router = self.router.with_state(self.state);
        TestApp {
            router,
            cleanup: Mutex::new(cleanup),
        }
    }
}

impl TestAppBuilder<()> {
    /// Convenience constructor for stateless test apps.
    pub fn http() -> Self {
        Self::new(())
    }
}

/// Status code helper for assertions.
pub fn status(response: &Response<Body>) -> StatusCode {
    response.status()
}

/// A wrapped HTTP test response providing fluent, expressive assertion methods.
#[derive(Debug)]
pub struct TestResponse {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl TestResponse {
    /// Creates a `TestResponse` by consuming an Axum HTTP `Response<Body>`.
    pub async fn from_response(response: Response<Body>) -> Self {
        let (parts, body) = response.into_parts();
        use axum::body::to_bytes;
        let bytes = to_bytes(body, 16 * 1024 * 1024)
            .await
            .expect("failed to read response body in test")
            .to_vec();
        Self {
            status: parts.status,
            headers: parts.headers,
            body: bytes,
        }
    }

    /// Returns the HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Returns the HTTP headers map.
    pub fn headers(&self) -> &axum::http::HeaderMap {
        &self.headers
    }

    /// Asserts that the response status equals `expected`.
    pub fn assert_status(self, expected: StatusCode) -> Self {
        assert_eq!(
            self.status, expected,
            "expected HTTP status {}, got {} with body: {}",
            expected,
            self.status,
            self.text()
        );
        self
    }

    /// Asserts that the response headers contain `name` with exact value `expected`.
    pub fn assert_header(self, name: &str, expected: &str) -> Self {
        let header_name = axum::http::HeaderName::from_bytes(name.as_bytes())
            .unwrap_or_else(|_| panic!("invalid header name '{name}'"));
        let val = self
            .headers
            .get(&header_name)
            .unwrap_or_else(|| panic!("header '{name}' not found in response"));
        let val_str = val.to_str().expect("valid utf-8 header value");
        assert_eq!(
            val_str, expected,
            "expected header '{name}' to equal '{expected}', got '{val_str}'"
        );
        self
    }

    /// Asserts that the JSON response body matches a dot-separated path query.
    ///
    /// For example, `res.assert_json_path("user.name", "Alice")` or `res.assert_json_path("status", 400)`.
    pub fn assert_json_path<V: serde::Serialize>(self, path: &str, expected: V) -> Self {
        let expected_val = serde_json::to_value(expected).expect("serialize expected value");
        let json_body: serde_json::Value =
            serde_json::from_slice(&self.body).expect("response body is not valid JSON");

        let mut current = &json_body;
        for segment in path.split('.') {
            current = match current {
                serde_json::Value::Object(map) => map
                    .get(segment)
                    .unwrap_or_else(|| panic!("path segment '{segment}' not found in JSON: {json_body}")),
                serde_json::Value::Array(arr) => {
                    let idx = segment
                        .parse::<usize>()
                        .unwrap_or_else(|_| panic!("expected array index at '{segment}' in path '{path}'"));
                    arr.get(idx)
                        .unwrap_or_else(|| panic!("array index '{idx}' out of bounds in JSON: {json_body}"))
                }
                _ => panic!("cannot navigate path segment '{segment}' on scalar in JSON: {json_body}"),
            };
        }

        assert_eq!(
            *current, expected_val,
            "expected JSON path '{path}' to equal {:?}, got {:?}",
            expected_val, current
        );
        self
    }

    /// Asserts that the response body text equals `expected`.
    pub fn assert_text_equals(self, expected: &str) -> Self {
        assert_eq!(
            self.text(),
            expected,
            "expected body '{}', got '{}'",
            expected,
            self.text()
        );
        self
    }

    /// Reads the response body as a UTF-8 string.
    pub fn text(&self) -> &str {
        std::str::from_utf8(&self.body).expect("response body is not valid UTF-8")
    }

    /// Reads the response body bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.body
    }

    /// Deserializes the response body JSON into typed `T`.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("failed to deserialize response body as JSON: {e}\nbody was: {}", self.text()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Message {
        text: String,
    }

    async fn hello_handler() -> axum::Json<Message> {
        axum::Json(Message {
            text: "hello".to_string(),
        })
    }

    #[tokio::test]
    async fn test_app_get_returns_200() {
        let router = axum::Router::new().route("/hello", get(hello_handler));
        let app = TestApp::new(router);

        let response = app.get("/hello").await;
        assert_eq!(response.status(), StatusCode::OK);

        let body = TestApp::body_text(response).await;
        assert!(body.contains("\"text\":\"hello\""));
    }

    #[tokio::test]
    async fn test_app_post_json_sends_body() {
        async fn echo_handler() -> &'static str {
            "received"
        }

        let router = axum::Router::new().route("/echo", axum::routing::post(echo_handler));
        let app = TestApp::new(router);

        let response = app.post_json("/echo", r#"{"text":"posted"}"#).await;
        assert_eq!(response.status(), StatusCode::OK);

        let body = TestApp::body_text(response).await;
        assert_eq!(body, "received");
    }

    #[tokio::test]
    async fn test_app_builder_http_works() {
        let router = axum::Router::new().route("/hi", get(hello_handler));
        let app = TestAppBuilder::http().router(router).build().await;

        let response = app.get("/hi").await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_app_builder_with_state() {
        let router = axum::Router::new()
            .route(
                "/state",
                get(
                    |axum::extract::State(count): axum::extract::State<usize>| async move {
                        format!("count: {count}")
                    },
                ),
            )
            .with_state(42usize);
        let app = TestApp::new(router);

        let response = app.get("/state").await;
        assert_eq!(response.status(), StatusCode::OK);

        let body = TestApp::body_text(response).await;
        assert!(body.contains("count: 42"));
    }

    #[test]
    fn test_app_cleanup_runs_on_drop() {
        let cleanup_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cleanup_ran_clone = cleanup_ran.clone();

        let router = axum::Router::new();
        let _app = TestApp::with_cleanup(router, move || {
            cleanup_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        assert!(!cleanup_ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn test_app_teardown_runs_cleanup() {
        let cleanup_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cleanup_ran_clone = cleanup_ran.clone();

        let router = axum::Router::new();
        let app = TestApp::with_cleanup(router, move || {
            cleanup_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        app.teardown();
        assert!(cleanup_ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_response_fluent_assertions() {
        let router = axum::Router::new().route(
            "/api/user",
            get(|| async {
                (
                    [("x-custom-header", "custom-value")],
                    axum::Json(serde_json::json!({
                        "user": {
                            "name": "Antigravity",
                            "roles": ["admin", "dev"]
                        },
                        "active": true
                    })),
                )
            }),
        );
        let app = TestApp::new(router);

        let res = app.get_res("/api/user").await;
        res.assert_status(StatusCode::OK)
            .assert_header("x-custom-header", "custom-value")
            .assert_json_path("user.name", "Antigravity")
            .assert_json_path("user.roles.0", "admin")
            .assert_json_path("active", true);
    }

    #[test]
    fn status_helper_returns_status_code() {
        let response = Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap();
        assert_eq!(status(&response), StatusCode::NOT_FOUND);
    }
}
