//! Real TCP HTTP server fixture and client for testing Rume applications over network sockets.

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Method, Request, Response, StatusCode};
use axum::Router;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::app::builder::{BuildError, RumeBuilder};
use crate::testing::app::TestResponse;

/// A running HTTP server bound to an ephemeral TCP port on `127.0.0.1`.
///
/// Unlike [`super::app::TestApp`], which routes in-memory requests via Tower's
/// `oneshot()` interface, `TestServer` runs a live TCP socket accept loop using
/// `axum::serve`, exercising real socket I/O, keep-alive connections, streaming,
/// and graceful shutdown.
pub struct TestServer {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    server_task: Option<tokio::task::JoinHandle<()>>,
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

        Ok(Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
            server_task: Some(server_task),
        })
    }

    /// Builds and starts a real HTTP server from a [`RumeBuilder`], executing module
    /// startup hooks and binding to an ephemeral TCP port.
    pub async fn from_builder<E, C, S>(
        builder: RumeBuilder<E, C, S>,
    ) -> Result<Self, BuildError>
    where
        E: Clone + Send + Sync + 'static,
        C: Clone + Send + Sync + 'static,
        S: Clone + Send + Sync + 'static,
    {
        let app = builder.build_test_app().await?;
        let router = app.into_router();
        Self::start(router).await.map_err(|e| {
            BuildError::Startup(format!("failed to bind test server TCP listener: {e}"))
        })
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

    /// Issues a real GET request over TCP, returning a fluent [`TestResponse`].
    pub async fn get(&self, path: &str) -> Result<TestResponse, std::io::Error> {
        let req = Request::builder()
            .method(Method::GET)
            .uri(path)
            .body(Body::empty())
            .map_err(std::io::Error::other)?;
        self.request(req).await
    }

    /// Issues a real POST request with a JSON body over TCP, returning a fluent [`TestResponse`].
    pub async fn post_json(
        &self,
        path: &str,
        body: impl Into<String>,
    ) -> Result<TestResponse, std::io::Error> {
        let body_str = body.into();
        let req = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body_str))
            .map_err(std::io::Error::other)?;
        self.request(req).await
    }

    /// Issues an arbitrary HTTP request over a live TCP connection and parses the response.
    pub async fn request(&self, req: Request<Body>) -> Result<TestResponse, std::io::Error> {
        let (parts, body) = req.into_parts();
        use axum::body::to_bytes;
        let body_bytes = to_bytes(body, 16 * 1024 * 1024)
            .await
            .map_err(std::io::Error::other)?;

        let mut stream = TcpStream::connect(self.addr).await?;
        stream.set_nodelay(true)?;

        let path_and_query = parts
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");

        let mut request_bytes = Vec::with_capacity(256 + body_bytes.len());
        request_bytes.extend_from_slice(
            format!("{} {} HTTP/1.1\r\n", parts.method.as_str(), path_and_query).as_bytes(),
        );
        request_bytes.extend_from_slice(format!("Host: {}\r\n", self.addr).as_bytes());
        request_bytes.extend_from_slice(b"Connection: close\r\n");

        if !parts.headers.contains_key("content-length") && !body_bytes.is_empty() {
            request_bytes.extend_from_slice(
                format!("Content-Length: {}\r\n", body_bytes.len()).as_bytes(),
            );
        }

        for (name, val) in parts.headers.iter() {
            request_bytes.extend_from_slice(name.as_str().as_bytes());
            request_bytes.extend_from_slice(b": ");
            request_bytes.extend_from_slice(val.as_bytes());
            request_bytes.extend_from_slice(b"\r\n");
        }
        request_bytes.extend_from_slice(b"\r\n");
        request_bytes.extend_from_slice(&body_bytes);

        stream.write_all(&request_bytes).await?;
        stream.flush().await?;

        // Read raw HTTP response until EOF
        let mut raw_response = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            raw_response.extend_from_slice(&chunk[..n]);
        }

        let resp = parse_raw_http_response(&raw_response)?;
        Ok(TestResponse::from_response(resp).await)
    }

    /// Explicitly and gracefully shuts down the server, draining active connections.
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
        Ok(())
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

fn parse_raw_http_response(raw: &[u8]) -> Result<Response<Body>, std::io::Error> {
    let boundary = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("incomplete HTTP response headers from test server"))?;
    let header_bytes = &raw[..boundary];
    let body_bytes = &raw[boundary + 4..];

    let header_str = std::str::from_utf8(header_bytes).map_err(std::io::Error::other)?;
    let mut lines = header_str.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| std::io::Error::other("empty response status line"))?;

    let parts: Vec<&str> = status_line.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return Err(std::io::Error::other("malformed status line"));
    }
    let code: u16 = parts[1].parse().map_err(std::io::Error::other)?;
    let status = StatusCode::from_u16(code).map_err(std::io::Error::other)?;

    let mut response = Response::builder().status(status);
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim();
            let val = v.trim();
            if let (Ok(h_key), Ok(h_val)) = (
                HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(val),
            ) {
                response = response.header(h_key, h_val);
            }
        }
    }

    response
        .body(Body::from(body_bytes.to_vec()))
        .map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use axum::Json;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct EchoMessage {
        message: String,
    }

    #[tokio::test]
    async fn test_server_ephemeral_tcp_lifecycle_and_requests() {
        let router = Router::new()
            .route("/ping", get(|| async { "pong" }))
            .route(
                "/echo",
                post(|Json(payload): Json<EchoMessage>| async move {
                    Json(EchoMessage {
                        message: format!("echo: {}", payload.message),
                    })
                }),
            );

        let server = TestServer::start(router).await.expect("start test server");
        assert!(server.local_addr().port() > 0);
        assert_eq!(
            server.url("/ping"),
            format!("http://{}/ping", server.local_addr())
        );

        // GET request over real TCP
        let resp = server.get("/ping").await.expect("get request succeeds");
        resp.assert_status(StatusCode::OK)
            .assert_text_equals("pong");

        // POST request over real TCP
        let post_resp = server
            .post_json("/echo", r#"{"message":"hello world"}"#)
            .await
            .expect("post request succeeds")
            .assert_status(StatusCode::OK)
            .assert_header("content-type", "application/json");

        let echo: EchoMessage = post_resp.json();
        assert_eq!(
            echo,
            EchoMessage {
                message: "echo: hello world".to_string()
            }
        );

        // Explicit shutdown
        server.shutdown().await.expect("shutdown test server");
    }
}
