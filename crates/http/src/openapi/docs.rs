//! OpenAPI documentation UI route helpers supporting Scalar and Swagger UI.
//!
//! # Content-Security-Policy (CSP)
//!
//! The default configuration loads the UI assets from a pinned `cdn.jsdelivr.net`
//! URL. When paired with `SecurityHeadersConfig::default()` (which sets
//! `Content-Security-Policy: default-src 'self'`), browsers will block the CDN
//! script and stylesheet unless you either allow the CDN origin in your CSP
//! policy or self-host the assets and point [`OpenApiDocsConfig::with_script_url`]
//! (and [`OpenApiDocsConfig::with_stylesheet_url`] for Swagger UI) at your own
//! origin.
//!
//! Note that [`OpenApiUi::Scalar`] emits no inline script and works with
//! `default-src 'self'` when self-hosted, whereas [`OpenApiUi::SwaggerUi`]
//! embeds an inline `<script>` bootstrap block (`SwaggerUIBundle(...)`) that
//! requires `'unsafe-inline'`, a CSP nonce, or a script hash even when the
//! bundle and stylesheet are self-hosted.

use axum::body::Bytes;
use axum::http::{StatusCode, header};
use rume_foundation::util::html::escape_html;

use crate::routing::Router;

/// Pinned CDN script URL for the default Scalar API Reference UI.
///
/// Why: pinned CDN versions guarantee reproducible docs rendering and prevent
/// supply-chain drift from mutable `@latest` tags; callers can override via
/// [`OpenApiDocsConfig::with_script_url`] for self-hosted deployments.
pub const DEFAULT_SCALAR_SCRIPT_URL: &str =
    "https://cdn.jsdelivr.net/npm/@scalar/api-reference@1.28.30";

/// Pinned CDN script URL for the Swagger UI bundle.
///
/// Why: pinned CDN versions guarantee reproducible docs rendering and prevent
/// supply-chain drift from mutable `@latest` tags; callers can override via
/// [`OpenApiDocsConfig::with_script_url`] for self-hosted deployments.
pub const DEFAULT_SWAGGER_UI_SCRIPT_URL: &str =
    "https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.20.1/swagger-ui-bundle.js";

/// Pinned CDN stylesheet URL for Swagger UI.
///
/// Why: pinned CDN versions guarantee reproducible docs rendering and prevent
/// supply-chain drift from mutable `@latest` tags; callers can override via
/// [`OpenApiDocsConfig::with_stylesheet_url`] for self-hosted deployments.
pub const DEFAULT_SWAGGER_UI_STYLESHEET_URL: &str =
    "https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.20.1/swagger-ui.css";

/// Selectable documentation UI renderer for OpenAPI specifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenApiUi {
    /// Scalar API Reference UI (default).
    #[default]
    Scalar,
    /// Swagger UI bundle.
    SwaggerUi,
}

/// Configuration for serving an OpenAPI documentation UI page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenApiDocsConfig {
    /// HTML document `<title>` (default: `"API Documentation"`).
    pub title: String,
    /// URL of the OpenAPI JSON specification (default: `"/openapi.json"`).
    pub spec_url: String,
    /// Documentation UI renderer (default: [`OpenApiUi::Scalar`]).
    pub ui: OpenApiUi,
    /// Optional override for the UI script URL (for self-hosted or air-gapped deployments).
    pub script_url: Option<String>,
    /// Optional override for the Swagger UI stylesheet URL (unused for Scalar).
    pub stylesheet_url: Option<String>,
    /// Optional Subresource Integrity (SRI) hash for the UI `<script>` tag.
    pub script_integrity: Option<String>,
    /// Optional Subresource Integrity (SRI) hash for the Swagger UI `<link rel="stylesheet">` tag.
    pub stylesheet_integrity: Option<String>,
}

impl Default for OpenApiDocsConfig {
    fn default() -> Self {
        Self {
            title: "API Documentation".to_string(),
            spec_url: "/openapi.json".to_string(),
            ui: OpenApiUi::default(),
            script_url: None,
            stylesheet_url: None,
            script_integrity: None,
            stylesheet_integrity: None,
        }
    }
}

/// Error returned when configuring [`OpenApiDocsConfig`] with an unsafe or invalid URL.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid OpenAPI docs {field}: must use https://, http://, or a relative / path")]
pub struct OpenApiDocsError {
    /// Name of the invalid configuration field.
    pub field: &'static str,
}

fn is_allowed_asset_url(url: &str) -> bool {
    let trimmed = url.trim();
    trimmed.starts_with("https://")
        || trimmed.starts_with("http://")
        || (trimmed.starts_with('/') && !trimmed.starts_with("//"))
}

impl OpenApiDocsConfig {
    /// Creates an OpenAPI docs configuration pointing at `spec_url`.
    #[must_use]
    pub fn new(spec_url: impl Into<String>) -> Self {
        Self {
            spec_url: spec_url.into(),
            ..Self::default()
        }
    }

    /// Sets the OpenAPI specification URL, rejecting unsafe schemes such as `javascript:` or `data:`.
    pub fn with_spec_url(mut self, spec_url: impl Into<String>) -> Result<Self, OpenApiDocsError> {
        let url = spec_url.into();
        if !is_allowed_asset_url(&url) {
            return Err(OpenApiDocsError { field: "spec_url" });
        }
        self.spec_url = url;
        Ok(self)
    }

    /// Sets the HTML document `<title>`.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Selects the OpenAPI documentation UI renderer.
    #[must_use]
    pub fn with_ui(mut self, ui: OpenApiUi) -> Self {
        self.ui = ui;
        self
    }

    /// Overrides the default pinned CDN `<script>` URL.
    ///
    /// Returns an error if `script_url` does not use `https://`, `http://`, or a relative `/` path.
    pub fn with_script_url(
        mut self,
        script_url: impl Into<String>,
    ) -> Result<Self, OpenApiDocsError> {
        let url = script_url.into();
        if !is_allowed_asset_url(&url) {
            return Err(OpenApiDocsError {
                field: "script_url",
            });
        }
        self.script_url = Some(url);
        Ok(self)
    }

    /// Overrides the default pinned CDN `<link rel="stylesheet">` URL (Swagger UI only).
    ///
    /// Returns an error if `stylesheet_url` does not use `https://`, `http://`, or a relative `/` path.
    pub fn with_stylesheet_url(
        mut self,
        stylesheet_url: impl Into<String>,
    ) -> Result<Self, OpenApiDocsError> {
        let url = stylesheet_url.into();
        if !is_allowed_asset_url(&url) {
            return Err(OpenApiDocsError {
                field: "stylesheet_url",
            });
        }
        self.stylesheet_url = Some(url);
        Ok(self)
    }

    /// Sets an optional Subresource Integrity (`integrity`) attribute for the `<script>` tag
    /// (also emits `crossorigin="anonymous"`).
    #[must_use]
    pub fn with_script_integrity(mut self, integrity: impl Into<String>) -> Self {
        self.script_integrity = Some(integrity.into());
        self
    }

    /// Sets an optional Subresource Integrity (`integrity`) attribute for the Swagger UI
    /// `<link rel="stylesheet">` tag (also emits `crossorigin="anonymous"`).
    #[must_use]
    pub fn with_stylesheet_integrity(mut self, integrity: impl Into<String>) -> Self {
        self.stylesheet_integrity = Some(integrity.into());
        self
    }
}

fn sri_attrs(integrity: Option<&str>) -> String {
    match integrity {
        Some(hash) if !hash.trim().is_empty() => format!(
            " integrity=\"{}\" crossorigin=\"anonymous\"",
            escape_html(hash.trim())
        ),
        _ => String::new(),
    }
}

fn safe_asset_url<'a>(configured: Option<&'a str>, default: &'a str) -> &'a str {
    match configured {
        Some(url) if is_allowed_asset_url(url) => url,
        _ => default,
    }
}

/// Encodes a string as a JavaScript string literal safe to embed inside an HTML `<script>` block.
// Why: `serde_json::to_string` escapes quotes, backslashes, and control characters,
// but leaves `<`, `>`, and `&` raw. Inside an HTML `<script>` tag, a raw `</script>`
// sequence terminates the script element even inside a quoted JS string. Replacing
// `<`, `>`, `&`, U+2028, and U+2029 with `\uXXXX` escapes keeps the literal valid
// JS while preventing script-tag breakout.
fn escape_js_string(value: &str) -> String {
    let json = serde_json::to_string(value).expect("string serialization cannot fail");
    json.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// Renders a safe HTML5 document for the configured OpenAPI documentation UI.
///
/// Escapes `title`, `spec_url`, `script_url`, and `stylesheet_url` for their
/// respective HTML/JS embedding contexts so untrusted or misconfigured values
/// cannot cause HTML or script injection.
///
/// # Content-Security-Policy (CSP)
///
/// The default CDN URLs require allowing `https://cdn.jsdelivr.net` in your CSP
/// when using `SecurityHeadersConfig::default()` (`default-src 'self'`), or
/// overriding [`OpenApiDocsConfig::with_script_url`] (and
/// [`OpenApiDocsConfig::with_stylesheet_url`] for Swagger UI) to serve
/// self-hosted assets from `'self'`. [`OpenApiUi::Scalar`] has no inline script
/// and works with `default-src 'self'` when self-hosted; [`OpenApiUi::SwaggerUi`]
/// embeds an inline `<script>` bootstrap block requiring `'unsafe-inline'`, a
/// nonce, or a hash even when self-hosted.
#[must_use]
pub fn openapi_docs_html(config: &OpenApiDocsConfig) -> String {
    let title = escape_html(&config.title);

    let safe_spec_url = safe_asset_url(Some(config.spec_url.as_str()), "/openapi.json");

    match config.ui {
        OpenApiUi::Scalar => {
            let spec_url = escape_html(safe_spec_url);
            let script_url = escape_html(safe_asset_url(
                config.script_url.as_deref(),
                DEFAULT_SCALAR_SCRIPT_URL,
            ));
            let script_sri = sri_attrs(config.script_integrity.as_deref());

            format!(
                concat!(
                    "<!doctype html>\n",
                    "<html lang=\"en\">\n",
                    "<head>\n",
                    "  <meta charset=\"utf-8\" />\n",
                    "  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n",
                    "  <title>{title}</title>\n",
                    "</head>\n",
                    "<body>\n",
                    "  <script id=\"api-reference\" data-url=\"{spec_url}\"></script>\n",
                    "  <script src=\"{script_url}\"{script_sri}></script>\n",
                    "</body>\n",
                    "</html>\n",
                ),
                title = title,
                spec_url = spec_url,
                script_url = script_url,
                script_sri = script_sri,
            )
        }
        OpenApiUi::SwaggerUi => {
            let spec_url_js = escape_js_string(safe_spec_url);
            let script_url = escape_html(safe_asset_url(
                config.script_url.as_deref(),
                DEFAULT_SWAGGER_UI_SCRIPT_URL,
            ));
            let stylesheet_url = escape_html(safe_asset_url(
                config.stylesheet_url.as_deref(),
                DEFAULT_SWAGGER_UI_STYLESHEET_URL,
            ));
            let script_sri = sri_attrs(config.script_integrity.as_deref());
            let stylesheet_sri = sri_attrs(config.stylesheet_integrity.as_deref());

            format!(
                concat!(
                    "<!doctype html>\n",
                    "<html lang=\"en\">\n",
                    "<head>\n",
                    "  <meta charset=\"utf-8\" />\n",
                    "  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n",
                    "  <title>{title}</title>\n",
                    "  <link rel=\"stylesheet\" href=\"{stylesheet_url}\"{stylesheet_sri} />\n",
                    "</head>\n",
                    "<body>\n",
                    "  <div id=\"swagger-ui\"></div>\n",
                    "  <script src=\"{script_url}\"{script_sri}></script>\n",
                    "  <script>\n",
                    "    window.onload = function () {{\n",
                    "      SwaggerUIBundle({{\n",
                    "        url: {spec_url_js},\n",
                    "        dom_id: \"#swagger-ui\",\n",
                    "      }});\n",
                    "    }};\n",
                    "  </script>\n",
                    "</body>\n",
                    "</html>\n",
                ),
                title = title,
                stylesheet_url = stylesheet_url,
                stylesheet_sri = stylesheet_sri,
                script_url = script_url,
                script_sri = script_sri,
                spec_url_js = spec_url_js,
            )
        }
    }
}

/// Mounts a `GET` route serving the configured OpenAPI documentation UI at `docs_path`.
///
/// Responds with `200 OK`, `Content-Type: text/html; charset=utf-8`, and
/// `Cache-Control: no-cache`. The HTML document is pre-rendered once at route
/// mount time so serving requests performs no per-request HTML formatting or
/// body allocation.
///
/// # Content-Security-Policy (CSP)
///
/// `SecurityHeadersConfig::default()` sets `default-src 'self'`, which blocks
/// the default pinned CDN script/stylesheet URLs. Either allow the CDN origin
/// in your CSP or configure [`OpenApiDocsConfig::with_script_url`] (and
/// [`OpenApiDocsConfig::with_stylesheet_url`] for Swagger UI) to point to
/// self-hosted assets. [`OpenApiUi::Scalar`] has no inline script and works
/// with `default-src 'self'` when self-hosted; [`OpenApiUi::SwaggerUi`] embeds
/// an inline `<script>` bootstrap block requiring `'unsafe-inline'`, a nonce,
/// or a hash even when self-hosted.
#[must_use]
pub fn openapi_docs_routes<S>(docs_path: &str, config: OpenApiDocsConfig) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    // Why: pre-render the HTML document once into a shared `Bytes` buffer at
    // route mount time instead of re-escaping and formatting on every request;
    // cloning `Bytes` is an atomic ref-count bump with zero per-request heap
    // allocation.
    let body = Bytes::from(openapi_docs_html(&config));

    Router::new().get(docs_path, move || {
        let body = body.clone();
        async move {
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                body,
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    use crate::openapi::docs::{
        DEFAULT_SCALAR_SCRIPT_URL, DEFAULT_SWAGGER_UI_SCRIPT_URL,
        DEFAULT_SWAGGER_UI_STYLESHEET_URL, OpenApiDocsConfig, OpenApiUi, openapi_docs_html,
        openapi_docs_routes,
    };

    #[test]
    fn openapi_docs_default_renders_scalar_with_pinned_cdn() {
        let config = OpenApiDocsConfig::default();
        assert_eq!(config.ui, OpenApiUi::Scalar);

        let html = openapi_docs_html(&config);
        assert!(html.contains(DEFAULT_SCALAR_SCRIPT_URL));
        assert!(html.contains(r#"data-url="/openapi.json""#));
        assert!(html.contains("<title>API Documentation</title>"));
    }

    #[test]
    fn openapi_docs_swagger_ui_renders_bundle_and_stylesheet() {
        let config = OpenApiDocsConfig::new("/api/openapi.json")
            .with_title("Orders API")
            .with_ui(OpenApiUi::SwaggerUi);

        let html = openapi_docs_html(&config);
        assert!(html.contains(DEFAULT_SWAGGER_UI_SCRIPT_URL));
        assert!(html.contains(DEFAULT_SWAGGER_UI_STYLESHEET_URL));
        assert!(html.contains("SwaggerUIBundle"));
        assert!(html.contains("/api/openapi.json"));
        assert!(html.contains("<title>Orders API</title>"));
    }

    #[test]
    fn openapi_docs_respects_custom_script_and_stylesheet_urls() {
        let scalar = OpenApiDocsConfig::default()
            .with_script_url("/static/scalar.js")
            .unwrap();
        let scalar_html = openapi_docs_html(&scalar);
        assert!(!scalar_html.contains(DEFAULT_SCALAR_SCRIPT_URL));
        assert!(scalar_html.contains(r#"src="/static/scalar.js""#));

        let swagger = OpenApiDocsConfig::default()
            .with_ui(OpenApiUi::SwaggerUi)
            .with_script_url("/static/swagger-ui-bundle.js")
            .unwrap()
            .with_stylesheet_url("/static/swagger-ui.css")
            .unwrap();
        let swagger_html = openapi_docs_html(&swagger);
        assert!(!swagger_html.contains(DEFAULT_SWAGGER_UI_SCRIPT_URL));
        assert!(!swagger_html.contains(DEFAULT_SWAGGER_UI_STYLESHEET_URL));
        assert!(swagger_html.contains(r#"src="/static/swagger-ui-bundle.js""#));
        assert!(swagger_html.contains(r#"href="/static/swagger-ui.css""#));
    }

    #[test]
    fn openapi_docs_escapes_title_and_spec_url_against_html_injection() {
        let evil_title = "<script>alert(1)</script>";
        let evil_spec = r#"/openapi.json"</script><img onerror=1>""#;
        let evil_script = r#"https://cdn.example/a.js" onload="alert(1)"#;
        let evil_css = r#"https://cdn.example/a.css" onload="alert(1)"#;

        let scalar = OpenApiDocsConfig::new(evil_spec)
            .with_title(evil_title)
            .with_script_url(evil_script)
            .unwrap();
        let scalar_html = openapi_docs_html(&scalar);
        assert!(!scalar_html.contains(evil_title));
        assert!(!scalar_html.contains("<script>alert(1)"));
        assert!(!scalar_html.contains("<img onerror=1>"));
        assert!(!scalar_html.contains(r#"onload="alert(1)""#));
        assert!(scalar_html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));

        let swagger = OpenApiDocsConfig::new(evil_spec)
            .with_title(evil_title)
            .with_ui(OpenApiUi::SwaggerUi)
            .with_script_url(evil_script)
            .unwrap()
            .with_stylesheet_url(evil_css)
            .unwrap();
        let swagger_html = openapi_docs_html(&swagger);
        assert!(!swagger_html.contains(evil_title));
        assert!(!swagger_html.contains("<script>alert(1)"));
        assert!(!swagger_html.contains("</script><img onerror=1>"));
        assert!(!swagger_html.contains("<img onerror=1>"));
        assert!(!swagger_html.contains(r#"onload="alert(1)""#));
        assert!(swagger_html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    }

    #[tokio::test]
    async fn openapi_docs_routes_serves_html_at_configured_path() {
        let config = OpenApiDocsConfig::new("/v1/openapi.json").with_title("V1 Docs");
        let expected_html = openapi_docs_html(&config);

        let router = openapi_docs_routes::<()>("/docs", config);
        let response = router
            .oneshot(Request::builder().uri("/docs").body(Body::empty()).unwrap())
            .await
            .expect("request failed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache"
        );

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(text, expected_html);
    }

    #[test]
    fn openapi_docs_emits_sri_and_falls_back_on_unsafe_direct_urls() {
        let cfg = OpenApiDocsConfig::default()
            .with_ui(OpenApiUi::SwaggerUi)
            .with_script_integrity("sha384-abc123")
            .with_stylesheet_integrity("sha384-def456");
        let html = openapi_docs_html(&cfg);
        assert!(html.contains(r#"integrity="sha384-abc123" crossorigin="anonymous""#));
        assert!(html.contains(r#"integrity="sha384-def456" crossorigin="anonymous""#));

        // Direct struct mutation with unsafe scheme falls back to pinned default
        let unsafe_cfg = OpenApiDocsConfig {
            spec_url: "javascript:alert(1)".to_string(),
            script_url: Some("javascript:alert(1)".to_string()),
            ..OpenApiDocsConfig::default()
        };
        let fallback_html = openapi_docs_html(&unsafe_cfg);
        assert!(!fallback_html.contains("javascript:"));
        assert!(fallback_html.contains(DEFAULT_SCALAR_SCRIPT_URL));
        assert!(fallback_html.contains(r#"data-url="/openapi.json""#));
    }

    #[test]
    fn openapi_docs_rejects_unsafe_urls_with_error() {
        assert!(
            OpenApiDocsConfig::default()
                .with_script_url("javascript:alert(1)")
                .is_err()
        );
        assert!(
            OpenApiDocsConfig::default()
                .with_stylesheet_url("data:text/css,body{}")
                .is_err()
        );
        assert!(
            OpenApiDocsConfig::default()
                .with_spec_url("javascript:alert(1)")
                .is_err()
        );
        assert!(
            OpenApiDocsConfig::default()
                .with_spec_url("data:application/json,{}")
                .is_err()
        );
    }
}
