# Task 02 — Native TLS, HTTP/2 & ACME

## Goal

Implement native TLS, HTTP/1.1 + HTTP/2 ALPN, and optional ACME certificate provisioning (`TlsConfig`) in `rume-http::tls` and wire `RumeBuilder::tls(TlsConfig)` into `Rume::run()`, `Rume::run_with_signal()`, and `Rume::run_with_listener_and_signal()` with clean graceful shutdown drain.

## Files touched

- `crates/http/src/tls/mod.rs` (and submodules `config.rs`, `acceptor.rs`, `acme.rs`)
- `crates/app/src/app/builder/mod.rs`
- `crates/app/src/app/builder/tests.rs`

## Deliverable

- `TlsConfig` in `rume-http::tls` (gated behind `tls` feature, with `AcmeConfig` / `TlsConfig::acme(...)` gated behind `acme`):
  - `TlsConfig::from_pem_files(cert_path, key_path)` and `TlsConfig::from_pem_bytes(cert_pem, key_pem)` parsing X.509 certificate chains and PKCS#8 / RSA / EC private keys via `rustls-pemfile` + `rustls-pki-types`.
  - `rustls::ServerConfig` configured with `alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()]`.
  - `TlsConfig::acme(AcmeConfig)` configuring `rustls-acme` (`AcmeConfig::new`, directory URL, domains, contact emails, cache dir) to produce a `tokio_rustls::TlsAcceptor` + background renewal driver.
- `RumeBuilder::tls(mut self, config: TlsConfig) -> Self` in `crates/app/src/app/builder/mod.rs`.
- `run_with_bound_listener` serves TLS connections when `TlsConfig` is set:
  - Spawns each `tls_acceptor.accept(stream)` in `tokio::spawn` with a bounded TLS handshake timeout (so a stalled handshake cannot block `TcpListener::accept()`).
  - Serves HTTP/1.1 + HTTP/2 (including HTTP/1.1 `Upgrade: websocket`) via `hyper_util::server::conn::auto::Builder::serve_connection_with_upgrades` wrapped in `hyper_util::server::graceful::GracefulShutdown`.
  - Injects `axum::extract::ConnectInfo<std::net::SocketAddr>` from the accepted peer address into request extensions (matching plaintext `axum::serve(...into_make_service_with_connect_info::<SocketAddr>())`).
  - Spawns any `rustls-acme` background renewal driver task alongside the server and aborts/stops it on shutdown, and logs `listening on https://{addr}` when TLS is active.
  - Transitions `LifecycleState::Terminating` and `AvailabilityState::RefusingTraffic` on shutdown signal and drains active connections within `shutdown_timeout` before invoking `run_shutdown`.

## Constraints honoured

- `crates/app/src/app/builder/mod.rs` (`RumeBuilder::run_with_listener_and_signal`, `AbortOnDrop` signal guard, `LifecycleState`, `AvailabilityState` transitions).
- `crates/app/src/app/lifecycle.rs` (`run_startup`, `run_shutdown`).
- `#![forbid(unsafe_code)]` across `rume-http` and `rume-app`.
- `rules/80-quality-bar.md` (zero-warning bar, fails-before TDD).

## Named tests (bodies omitted)

- `rume_http::tls::tests::tls_config_from_pem_bytes_negotiates_h2_and_http11_alpn` — generates a self-signed cert/key in-memory, builds `TlsConfig::from_pem_bytes`, and asserts ALPN includes `h2` and `http/1.1`.
- `rume_http::tls::tests::tls_config_rejects_invalid_pem_or_missing_key` — asserts descriptive error on malformed cert or missing private key.
- `rume_app::app::builder::tests::rume_builder_tls_serves_https_and_drains_gracefully_on_shutdown` — binds ephemeral `TcpListener`, runs `Rume::new().tls(...)` with a test route and shutdown oneshot, connects via `tokio_rustls` client, verifies HTTP/2 or HTTP/1.1 200 response, triggers shutdown, and asserts clean drain + `run_shutdown` completion.

## Verification

```bash
cargo test -j 2 -p rume-http -p rume-app --all-features
```
Expected: all TLS and builder tests pass with zero warnings.

## Commit

Only when the user asks:
```text
feat(http,app): add native TLS, HTTP/2 ALPN, ACME config, and graceful TLS drain
```

## Done-when

- [ ] `TlsConfig::from_pem_files`, `TlsConfig::from_pem_bytes`, and feature-gated `TlsConfig::acme` work and validate inputs cleanly.
- [ ] `RumeBuilder::tls(...)` serves HTTPS with HTTP/2 & HTTP/1.1 ALPN and drains in-flight TLS connections on shutdown.
- [ ] All `rume-http` and `rume-app` tests pass with `-D warnings`.
