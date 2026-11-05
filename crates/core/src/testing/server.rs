//! Real TCP HTTP server fixture and client for testing Rume applications over network sockets.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::app::builder::{BuildError, RumeBuilder};
use crate::testing::app::{TestApp, TestResponse};

/// A running HTTP server bound to an ephemeral TCP port on `127.0.0.1`.
///
/// Unlike [`super::app::TestApp`], which routes in-memory requests via Tower's
/// `oneshot()` interface, `TestServer` runs a live TCP socket accept loop using
/// `axum::serve`, exercising real socket I/O, keep-alive connections, streaming,
/// chunked responses, and graceful shutdown.
pub struct TestServer {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    server_task: Option<tokio::task::JoinHandle<()>>,
    client: reqwest::Client,
    app: Option<TestApp>,
}

impl TestServer {
    /// Starts a real HTTP server on an ephemeral TCP port (`127.0.0.1:0`) with the provided router.
    pub async fn start(router: impl Into<Router<()>>) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let router = router.into();

        let server_task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });

        let client = reqwest::Client::builder()
            .pool_idle_timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(10)
            .tcp_nodelay(true)
            .build()
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        Ok(Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
            server_task: Some(server_task),
            client,
            app: None,
        })
    }

    /// Starts a real HTTP server on an ephemeral TCP port (`127.0.0.1:0`) encapsulating
    /// a [`TestApp`], preserving its lifecycle guard and executing module shutdown hooks
    /// in reverse topological order only upon server shutdown or drop.
    pub async fn from_app(app: TestApp) -> Result<Self, std::io::Error> {
        let mut server = Self::start(app.router().clone()).await?;
        server.app = Some(app);
        Ok(server)
    }

    /// Builds and starts a real HTTP server from a [`RumeBuilder`], executing module
    /// startup hooks and binding to an ephemeral TCP port.
    ///
    /// The application lifecycle guard is preserved for the entire duration of the server;
    /// module `on_shutdown` hooks execute in reverse topological order only when [`TestServer::shutdown`]
    /// is awaited or when the server is dropped.
    pub async fn from_builder<E, C, S>(builder: RumeBuilder<E, C, S>) -> Result<Self, BuildError>
    where
        E: Clone + Send + Sync + 'static,
        C: Clone + Send + Sync + 'static,
        S: Clone + Send + Sync + 'static,
    {
        let app = builder.build_test_app().await?;
        Self::from_app(app).await.map_err(|e| {
            BuildError::Startup(format!("failed to bind test server TCP listener: {e}"))
        })
    }

    /// Returns a reference to the encapsulated [`TestApp`], if started from an application or builder.
    pub fn app(&self) -> Option<&TestApp> {
        self.app.as_ref()
    }

    /// Returns the local socket address the server is listening on.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Returns the fully qualified URL for a path (e.g. `http://127.0.0.1:45321/api/v1/health`).
    pub fn url(&self, path: &str) -> String {
        let path = if path.starts_with('/') {
            path
        } else {
            &format!("/{path}")
        };
        format!("http://{}{}", self.addr, path)
    }

    /// Returns a reference to the shared, connection-pooled HTTP [`reqwest::Client`].
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Connects a raw, unbuffered [`TcpStream`] to the server for low-level protocol tests.
    pub async fn connect_raw(&self) -> Result<TcpStream, std::io::Error> {
        let stream = TcpStream::connect(self.addr).await?;
        stream.set_nodelay(true)?;
        Ok(stream)
    }

    /// Issues a real GET request over TCP, returning a fluent [`TestResponse`].
    pub async fn get(&self, path: &str) -> Result<TestResponse, std::io::Error> {
        let url = self.url(path);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(std::io::Error::other)?;
        Self::from_reqwest_response(resp).await
    }

    /// Issues a real POST request with a JSON body over TCP, returning a fluent [`TestResponse`].
    pub async fn post_json(
        &self,
        path: &str,
        body: impl Into<String>,
    ) -> Result<TestResponse, std::io::Error> {
        let url = self.url(path);
        let resp = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .body(body.into())
            .send()
            .await
            .map_err(std::io::Error::other)?;
        Self::from_reqwest_response(resp).await
    }

    /// Issues an arbitrary HTTP request using the keep-alive connection pool and parses the response.
    pub async fn request(&self, req: Request<Body>) -> Result<TestResponse, std::io::Error> {
        let (parts, body) = req.into_parts();
        let path = parts
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let url = self.url(path);

        let body_bytes = axum::body::to_bytes(body, 64 * 1024 * 1024)
            .await
            .map_err(std::io::Error::other)?;

        let mut builder = self.client.request(parts.method, &url);
        for (name, val) in parts.headers.iter() {
            builder = builder.header(name, val);
        }
        if !body_bytes.is_empty() {
            builder = builder.body(body_bytes);
        }

        let resp = builder.send().await.map_err(std::io::Error::other)?;
        Self::from_reqwest_response(resp).await
    }

    /// Converts a [`reqwest::Response`] into a fluent [`TestResponse`].
    pub async fn from_reqwest_response(
        resp: reqwest::Response,
    ) -> Result<TestResponse, std::io::Error> {
        let status = StatusCode::from_u16(resp.status().as_u16())
            .map_err(|e| std::io::Error::other(format!("invalid status code: {e}")))?;

        let mut headers = axum::http::HeaderMap::new();
        for (name, val) in resp.headers().iter() {
            if let (Ok(hname), Ok(hval)) = (
                axum::http::HeaderName::from_bytes(name.as_str().as_bytes()),
                axum::http::HeaderValue::from_bytes(val.as_bytes()),
            ) {
                headers.insert(hname, hval);
            }
        }

        let bytes = resp.bytes().await.map_err(std::io::Error::other)?.to_vec();
        Ok(TestResponse::new(status, headers, bytes))
    }

    /// Explicitly and gracefully shuts down the server: stops accepting new connections,
    /// drains active in-flight requests, and runs application module teardown in reverse topological order.
    pub async fn shutdown(mut self) -> Result<(), std::io::Error> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.server_task.take() {
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "test server graceful shutdown timed out",
                    )
                })?
                .map_err(std::io::Error::other)?;
        }
        if let Some(app) = self.app.take() {
            app.teardown_async().await;
        }
        Ok(())
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(app) = self.app.take() {
            app.teardown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::routing::{get, post};
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct EchoRequest {
        message: String,
    }

    #[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
    struct EchoResponse {
        echo: String,
    }

    #[tokio::test]
    async fn test_server_ephemeral_tcp_lifecycle_and_requests() {
        let router = Router::new()
            .route(
                "/health",
                get(|| async {
                    (
                        StatusCode::OK,
                        [("x-service-status", "healthy")],
                        "all-systems-operational",
                    )
                }),
            )
            .route(
                "/api/echo",
                post(|Json(req): Json<EchoRequest>| async move {
                    Json(EchoResponse { echo: req.message })
                }),
            );

        let server = TestServer::start(router)
            .await
            .expect("server must start on ephemeral port");

        assert!(server.local_addr().port() > 0);

        let health_resp = server.get("/health").await.expect("GET request succeeds");
        health_resp
            .assert_status(StatusCode::OK)
            .assert_header("x-service-status", "healthy")
            .assert_text_equals("all-systems-operational");

        let echo_resp = server
            .post_json("/api/echo", r#"{"message":"hello over real tcp"}"#)
            .await
            .expect("POST request succeeds")
            .assert_status(StatusCode::OK);
        let echo_body: EchoResponse = echo_resp.json();
        assert_eq!(
            echo_body,
            EchoResponse {
                echo: "hello over real tcp".to_string(),
            }
        );

        server.shutdown().await.expect("graceful shutdown succeeds");
    }
}
