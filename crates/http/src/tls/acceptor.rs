//! TLS connection acceptor and HTTP/1.1 + HTTP/2 graceful drain server.

use axum::Router;
use axum::extract::ConnectInfo;
use axum::http::Request;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder as AutoConnBuilder;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tower::Service;

use super::TlsConfig;

/// RFC 8737 TLS-ALPN-01 challenge protocol identifier.
const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

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

/// Backoff duration after non-connection `accept()` errors (e.g. `EMFILE`/`ENFILE`)
/// to prevent 100% CPU spin-loops under file-descriptor exhaustion.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);

fn is_connection_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
    )
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
    // Why: configure explicit HTTP/1 header timeout and HTTP/2 concurrency/keep-alive
    // limits so slowloris and rapid-reset/stream-flood peers cannot exhaust connection
    // tasks or memory (`RC-20`).
    let mut conn_builder = AutoConnBuilder::new(TokioExecutor::new());
    conn_builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(30));
    conn_builder
        .http2()
        .timer(TokioTimer::new())
        .max_concurrent_streams(256)
        .keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(20));
    let handshake_timeout = tls_config.handshake_timeout;
    // Why: bound concurrent TLS handshakes and active connections so a client flood
    // cannot spawn unbounded Tokio tasks before handshakes time out (`RC-06`).
    let conn_semaphore = Arc::new(Semaphore::new(tls_config.max_connections));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    tokio::pin!(shutdown_signal);

    loop {
        let permit = tokio::select! {
            biased;
            () = &mut shutdown_signal => {
                break;
            }
            permit = Arc::clone(&conn_semaphore).acquire_owned() => {
                match permit {
                    Ok(permit) => permit,
                    Err(_) => break,
                }
            }
        };

        tokio::select! {
            biased;
            () = &mut shutdown_signal => {
                drop(permit);
                break;
            }
            accept_res = listener.accept() => {
                let (tcp_stream, peer_addr) = match accept_res {
                    Ok(pair) => pair,
                    Err(err) => {
                        drop(permit);
                        if is_connection_error(&err) {
                            tracing::debug!(error = %err, "TLS TCP connection accept error");
                        } else {
                            // Why: non-connection accept errors (`EMFILE`, `ENFILE`, etc.) repeat
                            // immediately on every loop iteration; back off briefly (while still
                            // honoring `shutdown_signal`) to avoid a 100% CPU spin-loop.
                            tracing::warn!(error = %err, "TLS TCP accept error; backing off");
                            tokio::select! {
                                biased;
                                () = &mut shutdown_signal => {
                                    break;
                                }
                                () = tokio::time::sleep(ACCEPT_ERROR_BACKOFF) => {}
                            }
                        }
                        continue;
                    }
                };

                let acceptor = acceptor.clone();
                let router = router.clone();
                let watcher = graceful.watcher();
                let conn_builder = conn_builder.clone();
                let mut task_shutdown = shutdown_rx.clone();

                tokio::spawn(async move {
                    let _permit = permit;
                    let mut tls_stream = tokio::select! {
                        biased;
                        _ = task_shutdown.changed() => {
                            tracing::debug!(peer = %peer_addr, "aborting TLS handshake due to shutdown");
                            return;
                        }
                        res = tokio::time::timeout(handshake_timeout, acceptor.accept(tcp_stream)) => {
                            match res {
                                Ok(Ok(stream)) => stream,
                                Ok(Err(err)) => {
                                    tracing::debug!(peer = %peer_addr, error = %err, "TLS handshake failed");
                                    return;
                                }
                                Err(_) => {
                                    tracing::debug!(peer = %peer_addr, "TLS handshake timed out");
                                    return;
                                }
                            }
                        }
                    };

                    // Why: per RFC 8737 §3, TLS-ALPN-01 validation connections negotiate
                    // `acme-tls/1` and MUST close immediately after the handshake without
                    // carrying HTTP application data (`RC-06`).
                    if tls_stream.get_ref().1.alpn_protocol() == Some(ACME_TLS_ALPN) {
                        tokio::select! {
                            biased;
                            _ = task_shutdown.changed() => {}
                            _ = tokio::time::timeout(handshake_timeout, tls_stream.shutdown()) => {}
                        }
                        return;
                    }

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

    // Why: drop `listener` and broadcast shutdown before draining so no new connections
    // are accepted and stalled TLS handshakes abort promptly (`RC-06`).
    drop(listener);
    let _ = shutdown_tx.send(true);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_connection_vs_resource_accept_errors() {
        assert!(is_connection_error(&std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "reset"
        )));
        assert!(is_connection_error(&std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            "aborted"
        )));
        assert!(is_connection_error(&std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused"
        )));
        assert!(!is_connection_error(&std::io::Error::other(
            "too many open files"
        )));
    }

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

    fn test_client_connector(alpn: Vec<Vec<u8>>) -> tokio_rustls::TlsConnector {
        use rustls_pki_types::pem::PemObject;
        let mut root_store = rustls::RootCertStore::empty();
        for cert in rustls_pki_types::CertificateDer::pem_slice_iter(TEST_CERT_PEM) {
            root_store.add(cert.unwrap()).unwrap();
        }
        let mut cfg = rustls::ClientConfig::builder_with_provider(
            super::super::config::default_crypto_provider(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(root_store)
        .with_no_client_auth();
        cfg.alpn_protocols = alpn;
        tokio_rustls::TlsConnector::from(Arc::new(cfg))
    }

    #[tokio::test]
    async fn acme_tls_alpn_closes_cleanly_without_invoking_http_router() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::AsyncReadExt;

        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = Arc::clone(&hits);
        let router = Router::new().route(
            "/",
            axum::routing::get(move || {
                let hits = Arc::clone(&hits_clone);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    "ok"
                }
            }),
        );

        let tls_cfg = TlsConfig::from_pem_bytes(TEST_CERT_PEM, TEST_KEY_PEM)
            .unwrap()
            .with_extra_alpn_for_tests(ACME_TLS_ALPN.to_vec());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            serve_tls_listener(
                listener,
                router,
                &tls_cfg,
                async move {
                    let _ = rx.await;
                },
                Duration::from_secs(2),
            )
            .await
        });

        let connector = test_client_connector(vec![ACME_TLS_ALPN.to_vec()]);
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let server_name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
        let mut tls_stream = connector.connect(server_name, tcp).await.unwrap();
        assert_eq!(tls_stream.get_ref().1.alpn_protocol(), Some(ACME_TLS_ALPN));

        // Server must close immediately without serving HTTP.
        let _ = tls_stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await;
        let mut buf = Vec::new();
        let _ = tls_stream.read_to_end(&mut buf).await;
        assert!(
            buf.is_empty(),
            "acme-tls/1 connection must not receive HTTP response bytes"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        let _ = tx.send(());
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn tls_acceptor_bounds_connections_and_aborts_stalled_handshakes_on_shutdown() {
        let router = Router::new().route("/", axum::routing::get(|| async { "ok" }));
        let tls_cfg = TlsConfig::from_pem_bytes(TEST_CERT_PEM, TEST_KEY_PEM)
            .unwrap()
            .max_connections(1)
            .handshake_timeout(Duration::from_secs(30));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            serve_tls_listener(
                listener,
                router,
                &tls_cfg,
                async move {
                    let _ = rx.await;
                },
                Duration::from_secs(2),
            )
            .await
        });

        // Open a TCP connection and never send a TLS ClientHello (stalled handshake holding the 1 permit).
        let _stalled = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Trigger shutdown; server must drop listener and abort the stalled handshake promptly
        // rather than waiting 30s for handshake_timeout.
        let _ = tx.send(());
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("server must shut down promptly without waiting for stalled handshake")
            .unwrap()
            .unwrap();
    }
}
