# Task 04 — Static File Serving, HTTP Caching & API Versioning

## Goal

Implement static file serving with SPA fallback (plus optional `rust-embed` integration), HTTP response caching & conditional request middleware (`ETag`, `If-None-Match`, `If-Match`, `Cache-Control`) backed by `rume_data::cache::Cache`, and API versioning (`/v1` route groups, header-based version matching, and RFC 8594 `Sunset`/`Deprecation` headers) in `rume-http` and `rume-app`.

## Files touched

- `crates/http/src/static_files/mod.rs`
- `crates/http/src/http_cache/mod.rs`
- `crates/http/src/routing/versioning.rs`, `crates/http/src/routing/mod.rs`
- `crates/app/src/app/builder/mod.rs`

## Deliverable

- **Static files (`rume-http::static_files`)**:
  - `StaticFilesConfig` (`root_dir`, `spa_fallback: Option<PathBuf>`, `cache_control`, `precompressed_gzip`, `precompressed_br`).
  - `static_files_router(config)` using `tower_http::services::{ServeDir, ServeFile}`.
  - Feature-gated `embedded_assets_router::<A: rust_embed::RustEmbed>(...)` under `static-embed`, serving embedded files with MIME type detection, `ETag`, and optional SPA `index.html` fallback.
- **HTTP caching (`rume-http::http_cache`)**:
  - `HttpCacheConfig` (`ttl`, `cache_control`, `weak_etags`, `vary_headers`).
  - `HttpCacheLayer` / `ConditionalRequestLayer` backed by `Arc<dyn rume_data::cache::Cache>`:
    - Caches `GET`/`HEAD` `200 OK` responses, computes deterministic `ETag`s, sets `Cache-Control`, returns `304 Not Modified` on matching `If-None-Match`, and enforces `If-Match` (`412 Precondition Failed`) on unsafe methods (`PUT`/`PATCH`/`DELETE`) when an ETag validator is provided.
- **API versioning (`rume-http::routing::versioning`)**:
  - `ApiVersion` (`v1`, `v2`, custom) + `versioned_group(version)` helper building prefixed `RouteGroup` (`/v1`, etc.).
  - `VersionHeaderLayer` / `ApiVersionExtractor` matching `X-API-Version` or `Accept: application/vnd.rume.v1+json` with `406 Not Acceptable` / `400 Bad Request` on unsupported versions.
  - `DeprecationLayer` attaching RFC 8594 `Deprecation` (e.g. `@<unix_ts>` or `true`) and `Sunset` (HTTP-date) + optional `Link: <url>; rel="sunset"` headers.

## Constraints honoured

- `crates/data/src/cache/mod.rs` (`Cache` trait).
- `crates/http/src/routing/group.rs` (`RouteGroup` prefixing conventions).
- `#![forbid(unsafe_code)]`, `rules/80-quality-bar.md`.

## Named tests (bodies omitted)

- `rume_http::static_files::tests::static_dir_serves_files_and_falls_back_to_spa_index` — creates a `tempfile::TempDir` with `app.js` and `index.html`, requests `/app.js` (200) and `/unknown/route` (200 `index.html` fallback).
- `rume_http::static_files::tests::embedded_assets_serve_with_mime_and_etag` — (under `static-embed`) serves an embedded asset and asserts `Content-Type` and `ETag`.
- `rume_http::http_cache::tests::http_cache_returns_304_on_matching_if_none_match_and_caches_in_backend` — calls handler twice (`200` with `ETag`, inner handler hit once), then sends `If-None-Match` and asserts `304 Not Modified`.
- `rume_http::http_cache::tests::conditional_if_match_returns_412_on_stale_etag` — sends `PUT` with stale `If-Match` and asserts `412 Precondition Failed`.
- `rume_http::routing::versioning::tests::versioned_routes_and_sunset_deprecation_headers_work` — tests `/v1` route group, header-based version matcher, and RFC 8594 `Deprecation` + `Sunset` headers on deprecated routes.

## Verification

```bash
cargo test -j 2 -p rume-http -p rume-app --all-features
```
Expected: all `static_files`, `http_cache`, `versioning`, and `rume-app` tests pass with zero warnings.

## Commit

Only when the user asks:
```text
feat(http,app): add static file serving, HTTP ETag caching, and API versioning with Sunset/Deprecation
```

## Done-when

- [ ] Static file serving + SPA fallback and feature-gated `rust-embed` integration work cleanly.
- [ ] `HttpCacheLayer` caches responses in `rume_data::cache::Cache` and handles `ETag`, `If-None-Match`, `If-Match`, and `Cache-Control`.
- [ ] API versioning (`/v1` groups, header matcher) and RFC 8594 `Sunset`/`Deprecation` headers are implemented and tested.
