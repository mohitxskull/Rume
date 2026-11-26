//! Static file serving with optional `rust-embed` for Rume.

use std::path::PathBuf;

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use tower_http::services::{ServeDir, ServeFile};

/// Configuration for serving static files from disk with optional SPA fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticFilesConfig {
    /// Root directory containing static assets.
    pub root_dir: PathBuf,
    /// Optional fallback file (e.g. `index.html`) for client-side SPA routing.
    pub spa_fallback: Option<PathBuf>,
    /// Optional `Cache-Control` header value applied when not already present.
    pub cache_control: Option<String>,
    /// Whether to serve precompressed `.gz` sibling files when accepted.
    pub precompressed_gzip: bool,
    /// Whether to serve precompressed `.br` sibling files when accepted.
    pub precompressed_br: bool,
}

impl StaticFilesConfig {
    /// Creates a static-file configuration rooted at `root_dir`.
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            spa_fallback: None,
            cache_control: None,
            precompressed_gzip: false,
            precompressed_br: false,
        }
    }

    /// Configures an SPA fallback file (typically `index.html`) for unmatched paths.
    pub fn with_spa_fallback(mut self, path: impl Into<PathBuf>) -> Self {
        self.spa_fallback = Some(path.into());
        self
    }

    /// Configures a default `Cache-Control` header for served static files.
    pub fn with_cache_control(mut self, value: impl Into<String>) -> Self {
        self.cache_control = Some(value.into());
        self
    }

    /// Enables or disables serving precompressed `.gz` assets.
    pub fn with_precompressed_gzip(mut self, enabled: bool) -> Self {
        self.precompressed_gzip = enabled;
        self
    }

    /// Enables or disables serving precompressed `.br` assets.
    pub fn with_precompressed_br(mut self, enabled: bool) -> Self {
        self.precompressed_br = enabled;
        self
    }
}

/// Builds an Axum [`Router`] that serves static files from `config.root_dir`.
pub fn static_files_router<S>(config: StaticFilesConfig) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let mut serve_dir = ServeDir::new(&config.root_dir);
    if config.precompressed_gzip {
        serve_dir = serve_dir.precompressed_gzip();
    }
    if config.precompressed_br {
        serve_dir = serve_dir.precompressed_br();
    }

    let router = match config.spa_fallback {
        Some(fallback) => {
            Router::new().fallback_service(serve_dir.fallback(ServeFile::new(fallback)))
        }
        None => Router::new().fallback_service(serve_dir),
    };

    apply_cache_control(router, config.cache_control)
}

fn apply_cache_control<S>(router: Router<S>, cache_control: Option<String>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let Some(cc) = cache_control else {
        return router;
    };
    let Ok(header_val) = HeaderValue::from_str(&cc) else {
        return router;
    };

    router.layer(middleware::from_fn(move |req: Request, next: Next| {
        let val = header_val.clone();
        async move {
            let mut res: Response = next.run(req).await;
            if res.status().is_success() && !res.headers().contains_key(header::CACHE_CONTROL) {
                res.headers_mut().insert(header::CACHE_CONTROL, val);
            }
            res
        }
    }))
}

/// Re-exported `rust_embed::RustEmbed` trait for `embedded_assets_router` consumers.
#[cfg(feature = "static-embed")]
pub use rust_embed::RustEmbed;

/// Configuration for serving compile-time embedded assets via `rust-embed`.
#[cfg(feature = "static-embed")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmbeddedAssetsConfig {
    /// Optional embedded fallback path (e.g. `"index.html"`) for SPA routing.
    pub spa_fallback: Option<String>,
    /// Optional `Cache-Control` header value applied to `200`/`304` responses.
    pub cache_control: Option<String>,
}

#[cfg(feature = "static-embed")]
impl EmbeddedAssetsConfig {
    /// Creates a default embedded-assets configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures an embedded SPA fallback path (e.g. `"index.html"`).
    pub fn with_spa_fallback(mut self, path: impl Into<String>) -> Self {
        self.spa_fallback = Some(path.into());
        self
    }

    /// Configures a `Cache-Control` header for embedded assets.
    pub fn with_cache_control(mut self, value: impl Into<String>) -> Self {
        self.cache_control = Some(value.into());
        self
    }
}

/// Builds an Axum [`Router`] serving embedded assets from `A: rust_embed::RustEmbed`.
///
/// Computes a deterministic SHA-256 `ETag`, sets `Content-Type` from the file extension,
/// handles `If-None-Match` (`304 Not Modified`), and optionally falls back to `spa_fallback`.
#[cfg(feature = "static-embed")]
pub fn embedded_assets_router<A, S>(config: EmbeddedAssetsConfig) -> Router<S>
where
    A: rust_embed::RustEmbed + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    use axum::body::Body;
    use axum::http::{Method, StatusCode};
    use axum::response::IntoResponse;
    use sha2::{Digest, Sha256};

    let spa_fallback = config.spa_fallback;
    let cache_control = config
        .cache_control
        .and_then(|cc| HeaderValue::from_str(&cc).ok());

    Router::new().fallback(move |req: Request| {
        let spa_fallback = spa_fallback.clone();
        let cache_control = cache_control.clone();
        async move {
            if *req.method() != Method::GET && *req.method() != Method::HEAD {
                return StatusCode::METHOD_NOT_ALLOWED.into_response();
            }

            let trimmed = req.uri().path().trim_start_matches('/');
            let (resolved_path, asset) = if !trimmed.is_empty() {
                if let Some(file) = A::get(trimmed) {
                    (trimmed.to_string(), file)
                } else if let Some(fb) = spa_fallback.as_deref() {
                    let fb_trimmed = fb.trim_start_matches('/');
                    match A::get(fb_trimmed) {
                        Some(file) => (fb_trimmed.to_string(), file),
                        None => return StatusCode::NOT_FOUND.into_response(),
                    }
                } else {
                    return StatusCode::NOT_FOUND.into_response();
                }
            } else if let Some(fb) = spa_fallback.as_deref() {
                let fb_trimmed = fb.trim_start_matches('/');
                match A::get(fb_trimmed) {
                    Some(file) => (fb_trimmed.to_string(), file),
                    None => return StatusCode::NOT_FOUND.into_response(),
                }
            } else if let Some(file) = A::get("index.html") {
                ("index.html".to_string(), file)
            } else {
                return StatusCode::NOT_FOUND.into_response();
            };

            let digest = Sha256::digest(asset.data.as_ref());
            let etag = format!("\"{}\"", hex::encode(digest));

            if let Some(inm) = req.headers().get(header::IF_NONE_MATCH)
                && let Ok(inm_str) = inm.to_str()
                && inm_str
                    .split(',')
                    .map(str::trim)
                    .any(|tag| tag == "*" || tag.trim_start_matches("W/") == etag)
            {
                let mut res = StatusCode::NOT_MODIFIED.into_response();
                if let Ok(val) = HeaderValue::from_str(&etag) {
                    res.headers_mut().insert(header::ETAG, val);
                }
                if let Some(cc) = cache_control {
                    res.headers_mut().insert(header::CACHE_CONTROL, cc);
                }
                return res;
            }

            let mime = mime_type_for_path(&resolved_path);
            let body = if *req.method() == Method::HEAD {
                Body::empty()
            } else {
                Body::from(asset.data.into_owned())
            };

            let mut res = Response::new(body);
            *res.status_mut() = StatusCode::OK;
            res.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
            if let Ok(val) = HeaderValue::from_str(&etag) {
                res.headers_mut().insert(header::ETAG, val);
            }
            if let Some(cc) = cache_control {
                res.headers_mut().insert(header::CACHE_CONTROL, cc);
            }
            res
        }
    })
}

#[cfg(feature = "static-embed")]
fn mime_type_for_path(path: &str) -> &'static str {
    let ext = path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "eot" => "application/vnd.ms-fontobject",
        "wasm" => "application/wasm",
        "txt" | "rs" | "md" => "text/plain; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    #[tokio::test]
    async fn static_dir_serves_files_and_falls_back_to_spa_index() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(dir.path().join("app.js"), "console.log('rume');").expect("write app.js");
        std::fs::write(dir.path().join("index.html"), "<html>spa</html>")
            .expect("write index.html");

        let config = StaticFilesConfig::new(dir.path())
            .with_spa_fallback(dir.path().join("index.html"))
            .with_cache_control("public, max-age=3600");
        let router: axum::Router<()> = static_files_router(config);

        let js_res = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/app.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(js_res.status(), StatusCode::OK);
        assert_eq!(
            js_res.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=3600"
        );
        let js_body = to_bytes(js_res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&js_body[..], b"console.log('rume');");

        let spa_res = router
            .oneshot(
                Request::builder()
                    .uri("/unknown/route")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(spa_res.status(), StatusCode::OK);
        let spa_body = to_bytes(spa_res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&spa_body[..], b"<html>spa</html>");
    }

    #[cfg(feature = "static-embed")]
    #[tokio::test]
    async fn embedded_assets_serve_with_mime_and_etag() {
        #[derive(rust_embed::RustEmbed)]
        #[folder = "src/static_files"]
        struct TestAssets;

        let router: axum::Router<()> = embedded_assets_router::<TestAssets, ()>(
            EmbeddedAssetsConfig::new().with_cache_control("public, max-age=60"),
        );

        let res = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/mod.rs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().get(header::CONTENT_TYPE).is_some());
        let etag = res
            .headers()
            .get(header::ETAG)
            .expect("missing ETag header")
            .to_str()
            .unwrap()
            .to_string();
        assert!(etag.starts_with('"') && etag.ends_with('"'));

        let not_modified = router
            .oneshot(
                Request::builder()
                    .uri("/mod.rs")
                    .header(header::IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
    }
}
