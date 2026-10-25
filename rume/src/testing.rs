//! Testing helpers for Rume applications.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use tower::ServiceExt;

/// A lightweight test application backed by an Axum router.
#[derive(Clone)]
pub struct TestApp {
    router: Router<()>,
}

impl TestApp {
    /// Create a test app from an already-built router.
    pub fn new(router: Router<()>) -> Self {
        Self { router }
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
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("failed to read body")
            .to_vec()
    }

    /// Read the response body as text.
    pub async fn body_text(response: Response<Body>) -> String {
        String::from_utf8(Self::body_bytes(response).await).expect("body is not utf-8")
    }
}

/// Builder for HTTP-only test applications.
pub struct TestAppBuilder<S> {
    state: S,
    router: Router<S>,
}

impl<S> TestAppBuilder<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub fn new(state: S) -> Self {
        Self {
            state,
            router: Router::new(),
        }
    }

    pub fn router(mut self, router: Router<S>) -> Self {
        self.router = router;
        self
    }

    pub fn route(mut self, path: &str, method_router: axum::routing::MethodRouter<S>) -> Self {
        self.router = self.router.route(path, method_router);
        self
    }

    pub async fn build(self) -> TestApp {
        TestApp::new(self.router.with_state(self.state))
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
