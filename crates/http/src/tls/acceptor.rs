//! TLS connection acceptor and HTTP/1.1 + HTTP/2 graceful drain server.

use axum::Router;
use axum::extract::ConnectInfo;
use axum::http::Request;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoConnBuilder;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use std::future::Future;
use std::net::SocketAddr;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::net::TcpListener;
use tower::Service;

use super::TlsConfig;

#[derive(Clone)]
struct ConnectInfoService {
    router: Router<()>,
    peer_addr: SocketAddr,
}

impl<B> Service<Request<B>> for ConnectInfoService
where
    Router<()>: Service<Request<B>>,
{
    type Response = <Router<()> as Service<Request<B>>>::Response;
    type Error = <Router<()> as Service<Request<B>>>::Error;
    type Future = <Router<()> as Service<Request<B>>>::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.router.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        req.extensions_mut().insert(ConnectInfo(self.peer_addr));
        self.router.call(req)
    }
}

struct AbortOnDrop(Option<tokio::task::JoinHandle<()>>);

impl AbortOnDrop {
    fn abort(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.abort();
    }
}

/// Serve an Axum router over TLS on a pre-bound [`TcpListener`] with graceful shutdown drain.
pub async fn serve_tls_listener<F>(
    listener: TcpListener,
    router: Router<()>,
    tls_config: &TlsConfig,
    shutdown_signal: F,
    shutdown_timeout: Duration,
) -> Result<(), std::io::Error>
where
    F: Future<Output = ()> + Send,
{
    let (acceptor, acme_driver) = tls_config
        .build_acceptor()
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err.to_string()))?;
    let mut acme_guard = AbortOnDrop(acme_driver);

    let graceful = GracefulShutdown::new();
    let conn_builder = AutoConnBuilder::new(TokioExecutor::new());
    let handshake_timeout = tls_config.handshake_timeout;

    tokio::pin!(shutdown_signal);

    loop {
        tokio::select! {
            biased;
            () = &mut shutdown_signal => {
                break;
            }
            accept_res = listener.accept() => {
                let (tcp_stream, peer_addr) = match accept_res {
                    Ok(pair) => pair,
                    Err(err) => {
                        tracing::warn!(error = %err, "TLS TCP accept error");
                        continue;
                    }
                };

                let acceptor = acceptor.clone();
                let router = router.clone();
                let watcher = graceful.watcher();
                let conn_builder = conn_builder.clone();

                tokio::spawn(async move {
                    let tls_stream = match tokio::time::timeout(
                        handshake_timeout,
                        acceptor.accept(tcp_stream),
                    )
                    .await
                    {
                        Ok(Ok(stream)) => stream,
                        Ok(Err(err)) => {
                            tracing::debug!(peer = %peer_addr, error = %err, "TLS handshake failed");
                            return;
                        }
                        Err(_) => {
                            tracing::debug!(peer = %peer_addr, "TLS handshake timed out");
                            return;
                        }
                    };

                    let io = TokioIo::new(tls_stream);
                    let service = TowerToHyperService::new(ConnectInfoService { router, peer_addr });
                    let conn = conn_builder.serve_connection_with_upgrades(io, service);
                    if let Err(err) = watcher.watch(conn).await {
                        tracing::debug!(peer = %peer_addr, error = %err, "TLS HTTP connection error");
                    }
                });
            }
        }
    }

    acme_guard.abort();

    if tokio::time::timeout(shutdown_timeout, graceful.shutdown())
        .await
        .is_err()
    {
        tracing::warn!(
            timeout_secs = shutdown_timeout.as_secs_f64(),
            "TLS graceful shutdown drain timed out; forcing remaining connections closed"
        );
    }

    Ok(())
}
