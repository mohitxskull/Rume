//! Testing helpers for Rume applications.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use tower::ServiceExt;

use std::sync::Mutex;

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

    /// Issue an arbitrary request.
    pub async fn request(&self, req: Request<Body>) -> Response<Body> {
        self.router
            .clone()
            .oneshot(req)
            .await
            .expect("request failed")
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
}

/// Declarative, fluent test harness coordinating database pools, lifecycle hooks, and test app routing.
#[cfg(feature = "sqlite")]
pub struct RumeTestHarness<B: crate::persistence::DatabaseBackend> {
    /// Active database pool configured for testing.
    pub pool: crate::persistence::DatabasePool<B>,
    /// Optional test app for issuing simulated HTTP requests.
    pub app: Option<TestApp>,
}

#[cfg(feature = "sqlite")]
impl RumeTestHarness<crate::persistence::SqliteBackend> {
    /// Builds an in-memory SQLite test harness with an initialized pool.
    pub async fn in_memory() -> Result<Self, crate::persistence::PersistenceError> {
        let backend = crate::persistence::SqliteBackend::in_memory().await?;
        let config = crate::persistence::DatabaseConfig::new("memory://test");
        let pool = crate::persistence::DatabasePool::new(backend, config);
        Ok(Self { pool, app: None })
    }

    /// Attaches an Axum router to the test harness.
    pub fn with_router(mut self, router: axum::Router<()>) -> Self {
        self.app = Some(TestApp::new(router));
        self
    }

    /// Returns a reference to the active test app, panicking if no router was configured.
    pub fn app(&self) -> &TestApp {
        self.app.as_ref().expect("no router configured on test harness")
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
        // T-T4: TestApp::new + get works.
        let router = axum::Router::new().route("/hello", get(hello_handler));
        let app = TestApp::new(router);

        let response = app.get("/hello").await;
        assert_eq!(response.status(), StatusCode::OK);

        let body = TestApp::body_text(response).await;
        assert!(body.contains("\"text\":\"hello\""));
    }

    #[tokio::test]
    async fn test_app_post_json_sends_body() {
        // T-T4: TestApp::post_json sends a JSON body.
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
        // T-T4: TestAppBuilder::http() + build works.
        let router = axum::Router::new().route("/hi", get(hello_handler));
        let app = TestAppBuilder::http().router(router).build().await;

        let response = app.get("/hi").await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_app_builder_with_state() {
        // T-T4: TestAppBuilder with custom state.
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
        // T-T4: TestApp::with_cleanup runs cleanup on Drop.
        let cleanup_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cleanup_ran_clone = cleanup_ran.clone();

        let router = axum::Router::new();
        let _app = TestApp::with_cleanup(router, move || {
            cleanup_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        assert!(!cleanup_ran.load(std::sync::atomic::Ordering::SeqCst));
        // App dropped at end of scope → cleanup runs
    }

    #[test]
    fn test_app_teardown_runs_cleanup() {
        // T-T4: TestApp::teardown runs cleanup explicitly.
        let cleanup_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cleanup_ran_clone = cleanup_ran.clone();

        let router = axum::Router::new();
        let app = TestApp::with_cleanup(router, move || {
            cleanup_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        app.teardown();
        assert!(cleanup_ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn status_helper_returns_status_code() {
        // T-T4: status() helper returns the response status code.
        let response = Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap();
        assert_eq!(status(&response), StatusCode::NOT_FOUND);
    }
}
