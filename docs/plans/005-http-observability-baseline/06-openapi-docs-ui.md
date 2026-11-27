# Task 06 — OpenAPI Docs UI: Scalar & Swagger UI

## Goal

Implement a lightweight, configurable OpenAPI Docs UI route helper in `rume-http::openapi::docs` supporting selectable `OpenApiUi::Scalar` (default) and `OpenApiUi::SwaggerUi`, serving minimal HTML pointing to a pinned CDN URL with a configurable `script_url` override for self-hosted or air-gapped deployments.

## Files touched

- `crates/http/src/openapi/docs.rs`

## Deliverable

1. **`OpenApiUi` enum (`crates/http/src/openapi/docs.rs`)**:
   - `#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)] pub enum OpenApiUi { #[default] Scalar, SwaggerUi }`.
2. **`OpenApiDocsConfig` builder**:
   - `title: String` (default `"API Documentation"`).
   - `spec_url: String` (default `"/openapi.json"`).
   - `ui: OpenApiUi` (default `OpenApiUi::Scalar`).
   - `script_url: Option<String>` (overrides the pinned default CDN `<script>` URL).
   - `stylesheet_url: Option<String>` (overrides the pinned default CDN `<link rel="stylesheet">` URL for Swagger UI; unused for Scalar).
   - Fluent builders: `OpenApiDocsConfig::new(spec_url)`, `.with_title(...)`, `.with_ui(OpenApiUi)`, `.with_script_url(...)`, `.with_stylesheet_url(...)`.
3. **Safe HTML renderer (`openapi_docs_html(&OpenApiDocsConfig) -> String`)**:
   - Escapes `title`, `spec_url`, `script_url`, and `stylesheet_url` using `rume_foundation::util::html::escape_html` (or equivalent HTML-attribute/JSON-string escaping) so untrusted/misconfigured strings can NEVER cause HTML/XSS injection.
   - For `OpenApiUi::Scalar`: renders minimal HTML5 document with `<script id="api-reference" data-url="{escaped_spec_url}"></script>` and `<script src="{pinned_scalar_cdn_or_override}"></script>`.
   - For `OpenApiUi::SwaggerUi`: renders minimal HTML5 document loading pinned Swagger UI bundle JS + CSS (or overrides) and initializing `SwaggerUIBundle({ url: <json-encoded spec_url>, dom_id: "#swagger-ui" })`.
   - Rustdoc explicitly notes CSP compatibility with `SecurityHeadersConfig::default()` (`default-src 'self'`), guiding users to either allow the CDN origin in CSP or configure `.with_script_url(...)` to a self-hosted asset.
4. **Route helper (`openapi_docs_routes<S>(docs_path: &str, config: OpenApiDocsConfig) -> crate::routing::Router<S>`)**:
   - Mounts `GET {docs_path}` returning `200 OK`, `Content-Type: text/html; charset=utf-8`, and `Cache-Control: no-cache` with the pre-rendered HTML body (pre-rendered once at route construction time for zero per-request allocation).

## Constraints honoured

- Approved decision #3: selectable `OpenApiUi` enum (`Scalar` default + `SwaggerUi`), minimal HTML with pinned CDN URL + configurable `script_url` override (no multi-MB embedded JS bundles).
- `#![forbid(unsafe_code)]` (`crates/http/src/lib.rs:3`).
- `rules/85-minimalism.md`: reuse `rume_foundation::util` HTML escaping and `serde_json::to_string` for JS string literal escaping; zero new crate dependencies.
- `rules/70-code-rationale.md`: document `Why:` on pinned CDN versions and pre-rendering HTML once at route mount time.

## Code-only decisions

- Pin explicit CDN versions in constants (no `@latest` unpinned CDN URLs):
  - `pub const DEFAULT_SCALAR_SCRIPT_URL: &str = "https://cdn.jsdelivr.net/npm/@scalar/api-reference@1.28.30";`
  - `pub const DEFAULT_SWAGGER_UI_SCRIPT_URL: &str = "https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.20.1/swagger-ui-bundle.js";`
  - `pub const DEFAULT_SWAGGER_UI_STYLESHEET_URL: &str = "https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.20.1/swagger-ui.css";`
  - Why: pinned CDN versions guarantee reproducible docs rendering and prevent supply-chain drift from mutable `@latest` tags; callers can override via `with_script_url` / `with_stylesheet_url` for self-hosted deployments.

## Named tests (bodies omitted)

In `crates/http/src/openapi/docs.rs`:
- `openapi_docs_default_renders_scalar_with_pinned_cdn` — asserts default config uses `OpenApiUi::Scalar`, includes `DEFAULT_SCALAR_SCRIPT_URL` and `data-url="/openapi.json"`.
- `openapi_docs_swagger_ui_renders_bundle_and_stylesheet` — configures `OpenApiUi::SwaggerUi`, asserts HTML includes `DEFAULT_SWAGGER_UI_SCRIPT_URL`, `DEFAULT_SWAGGER_UI_STYLESHEET_URL`, and `SwaggerUIBundle`.
- `openapi_docs_respects_custom_script_and_stylesheet_urls` — overrides `script_url` (and `stylesheet_url` for Swagger UI), asserts default CDN URLs do NOT appear and custom URLs DO appear.
- `openapi_docs_escapes_title_and_spec_url_against_html_injection` — passes `<script>alert(1)</script>` in `title` and `"</script><img onerror=1>"` in `spec_url`; asserts raw `<script>` / unescaped quotes are absent from rendered HTML.
- `openapi_docs_routes_serves_html_at_configured_path` — mounts `openapi_docs_routes::<()>("/docs", ...)` and sends `GET /docs` via `oneshot`, asserts `200 OK`, `Content-Type: text/html; charset=utf-8`, and expected HTML body.

## Verification

```bash
cargo fmt --all --check
cargo clippy -j 2 -p rume-http --all-targets --all-features -- -D warnings
cargo test -j 2 -p rume-http --lib --all-features openapi::docs::
```

Expected: zero warnings; all `openapi::docs::` tests pass.

## Commit

*Only when the user asks:*

```text
feat(http): add OpenAPI Docs UI route helper supporting Scalar and Swagger UI
```

## Done-when

- [ ] `OpenApiUi`, `OpenApiDocsConfig`, `openapi_docs_html`, and `openapi_docs_routes` are implemented with zero `todo!()` stubs remaining.
- [ ] All 5 named tests pass under `cargo test -j 2 -p rume-http --lib --all-features openapi::docs::`.
- [ ] `cargo clippy -j 2 -p rume-http --all-targets --all-features -- -D warnings` passes with zero warnings.
