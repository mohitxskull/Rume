# Task 02 — HTTP Transport Middleware: CORS, Compression & Body Limits

## Goal

Implement Rume's typed HTTP transport middleware in `rume-http::transport` — CORS, response compression (gzip, brotli, zstd), and transport-level request body-size limiting — backed by `tower-http = "=0.6.11"`, with fail-closed configuration validation and full unit tests.

## Files touched

- `crates/http/src/transport/mod.rs`
- `crates/http/src/transport/cors.rs`
- `crates/http/src/transport/compression.rs`
- `crates/http/src/transport/body_limit.rs`

## Deliverable

1. **CORS (`crates/http/src/transport/cors.rs`)**:
   - `CorsConfig` builder in Rume style (mirroring `crates/security/src/security/headers.rs`):
     - `allow_origins` (explicit origin list or explicit `AllowAnyOrigin` opt-in; default: **empty / deny all cross-origin** — fail closed).
     - `allow_methods`, `allow_headers`, `expose_headers`, `allow_credentials` (default `false`), `max_age` (optional `Duration`).
   - `CorsConfig::build_layer(&self) -> Result<tower_http::cors::CorsLayer, CorsConfigError>` (plus `CorsLayer` type alias or wrapper):
     - **Fail closed** on invalid CORS combinations per the Fetch spec and `tower-http 0.6` invariants:
       - Reject `allow_credentials = true` combined with wildcard `*` origin, wildcard `*` method, wildcard `*` header, or `expose_headers(Any)` (all of which panic at runtime in `tower-http 0.6`).
       - Reject literal `"*"` entries in `allow_origins`, `allow_methods`, `allow_headers`, or `expose_headers` string lists (requiring the explicit wildcard opt-in so `HeaderValue::from_str("*")` cannot bypass the credentials check).
       - Reject malformed `HeaderValue` / `HeaderName` / `Method` inputs with a typed `CorsConfigError`.
2. **Response Compression (`crates/http/src/transport/compression.rs`)**:
   - `CompressionConfig` builder:
     - Independent toggles for `gzip` (default `true`), `brotli` (default `true`), `zstd` (default `true`).
     - Optional `min_size_bytes` predicate (default `1024` bytes, skipping tiny responses where compression adds overhead; uses `tower_http::compression::predicate::SizeAbove`).
   - `CompressionConfig::build_layer(&self)` returning a configured `tower_http::compression::CompressionLayer` (or Rume wrapper implementing `tower::Layer`).
   - Respects `Accept-Encoding` negotiation (`gzip`, `br`, `zstd`) and sets `Vary: accept-encoding` and `Content-Encoding` appropriately.
3. **Request Body-Size Limits (`crates/http/src/transport/body_limit.rs`)**:
   - `BodyLimitConfig` with `max_bytes: usize` (default `DEFAULT_BODY_LIMIT_BYTES = 2 * 1024 * 1024`, i.e. 2 MiB).
   - `BodyLimitConfig::build_layer(&self)` returning `tower_http::limit::RequestBodyLimitLayer` (or Rume wrapper).
   - Enforces the limit at the transport layer, returning `413 Payload Too Large` both when `Content-Length` exceeds `max_bytes` and when a chunked/streaming body without `Content-Length` exceeds `max_bytes`.

## Constraints honoured

- `#![forbid(unsafe_code)]` (`crates/http/src/lib.rs:3`).
- Fail-closed security posture (matches `crates/security/src/security/headers.rs` and `crates/app/src/app/builder/middleware.rs`): default `CorsConfig` permits no cross-origin requests; wildcard + credentials returns `Err(CorsConfigError)` rather than panicking at runtime.
- `rules/70-code-rationale.md`: record `Why:` comments on `DEFAULT_BODY_LIMIT_BYTES` (2 MiB) and the default `1024`-byte compression threshold.
- `rules/85-minimalism.md`: delegate protocol mechanics to `tower-http` rather than re-implementing CORS headers or compression streams.

## Named tests (bodies omitted)

All unit tests live in `#[cfg(test)] mod tests` within their respective modules (`cors.rs`, `compression.rs`, `body_limit.rs`) using `axum::Router` + `tower::ServiceExt::oneshot`:

- `cors_default_denies_cross_origin_requests` — verifies no `Access-Control-Allow-Origin` header is emitted for an unconfigured origin.
- `cors_allows_configured_origin_and_preflight_headers` — sends `OPTIONS` preflight with `Origin` and `Access-Control-Request-Method`, asserts `200`/`204` and matching `Access-Control-Allow-Origin`, `Access-Control-Allow-Methods`, and `Access-Control-Max-Age`.
- `cors_rejects_wildcard_origin_with_credentials` — asserts `build_layer()` returns `Err(CorsConfigError::...)` instead of panicking or permitting `*` + `allow_credentials(true)`.
- `cors_rejects_wildcard_expose_headers_with_credentials_and_literal_star_in_lists` — asserts `expose_headers(Any)` + `allow_credentials(true)` returns `Err(CorsConfigError::...)`, and passing literal `"*"` in `allow_origins(["*"])`, `allow_methods(["*"])`, `allow_headers(["*"])`, or `expose_headers(["*"])` returns `Err(CorsConfigError::...)`.
- `cors_rejects_invalid_origin_header_value` — asserts CRLF / non-ASCII invalid origin fails closed with `CorsConfigError`.
- `compression_encodes_gzip_brotli_and_zstd_when_accepted` — sends requests with `Accept-Encoding: gzip`, `br`, and `zstd` for a body > 1 KiB, asserts matching `Content-Encoding` header and that the decompressed payload equals the original body.
- `compression_skips_small_responses_below_threshold` — sends `Accept-Encoding: gzip` for a tiny body (< `min_size_bytes`), asserts no `Content-Encoding` header is set.
- `compression_respects_disabled_algorithms` — disables `zstd` in `CompressionConfig`, sends `Accept-Encoding: zstd`, asserts response is uncompressed.
- `body_limit_allows_payload_within_limit_and_rejects_oversized_with_413` — tests exact boundary (`== max_bytes` returns `200`) and `max_bytes + 1` returns `413 Payload Too Large`.
- `body_limit_rejects_streaming_body_without_content_length` — sends a chunked/streamed body exceeding `max_bytes` with no `Content-Length` header, asserts `413 Payload Too Large`.

## Verification

```bash
cargo fmt --all --check
cargo clippy -j 2 -p rume-http --all-targets --all-features -- -D warnings
cargo test -j 2 -p rume-http --lib --all-features transport::
```

Expected: zero clippy warnings; all `transport::` tests pass.

## Commit

*Only when the user asks:*

```text
feat(http): add typed CORS, compression, and body-limit transport middleware
```

## Done-when

- [ ] `CorsConfig`, `CompressionConfig`, and `BodyLimitConfig` (plus layer builders) are implemented with zero `todo!()` stubs remaining.
- [ ] All 10 named tests pass under `cargo test -j 2 -p rume-http --lib --all-features transport::`.
- [ ] `cargo clippy -j 2 -p rume-http --all-targets --all-features -- -D warnings` passes with zero warnings.
