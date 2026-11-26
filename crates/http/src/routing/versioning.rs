//! API versioning and RFC 8594 `Sunset`/`Deprecation` support for Rume routing.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tower::{Layer, Service};

use crate::routing::group::RouteGroup;

static X_API_VERSION: HeaderName = HeaderName::from_static("x-api-version");
static DEPRECATION_HEADER: HeaderName = HeaderName::from_static("deprecation");
static SUNSET_HEADER: HeaderName = HeaderName::from_static("sunset");

/// Supported API version identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ApiVersion {
    /// `/v1`
    V1,
    /// `/v2`
    V2,
    /// Custom version segment (e.g. `"v3"`, `"2026-01-01"`).
    Custom(String),
}

impl ApiVersion {
    /// Returns [`ApiVersion::V1`].
    pub fn v1() -> Self {
        Self::V1
    }

    /// Returns [`ApiVersion::V2`].
    pub fn v2() -> Self {
        Self::V2
    }

    /// Creates a custom API version from `version` (leading `/` is stripped).
    pub fn custom(version: impl Into<String>) -> Self {
        let trimmed = version.into().trim().trim_start_matches('/').to_string();
        match trimmed.as_str() {
            "v1" | "1" => Self::V1,
            "v2" | "2" => Self::V2,
            _ => Self::Custom(trimmed),
        }
    }

    /// Returns the canonical version string (e.g. `"v1"`, `"v2"`).
    pub fn as_str(&self) -> &str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
            Self::Custom(s) => s.as_str(),
        }
    }

    /// Returns the URL path prefix for this version (e.g. `"/v1"`).
    pub fn prefix(&self) -> String {
        format!("/{}", self.as_str())
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ApiVersion {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty()
            || !trimmed
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
        {
            return Err(());
        }
        Ok(match trimmed {
            "v1" | "1" => Self::V1,
            "v2" | "2" => Self::V2,
            other => Self::Custom(other.to_string()),
        })
    }
}

/// Creates a [`RouteGroup`] prefixed with `version.prefix()` (e.g. `"/v1"`).
pub fn versioned_group<S>(version: ApiVersion) -> RouteGroup<S>
where
    S: Clone + Send + Sync + 'static,
{
    RouteGroup::new().prefix(&version.prefix())
}

/// Axum extractor yielding the negotiated [`ApiVersion`] from request extensions
/// or `X-API-Version` / `Accept: application/vnd.rume.<ver>+json` headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiVersionExtractor(pub ApiVersion);

impl<S> FromRequestParts<S> for ApiVersionExtractor
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if let Some(ext) = parts.extensions.get::<ApiVersion>() {
            return Ok(Self(ext.clone()));
        }
        match parse_version_from_headers(&parts.headers) {
            Ok(Some(ver)) => Ok(Self(ver)),
            Ok(None) | Err(()) => Err((StatusCode::BAD_REQUEST, "invalid or missing API version")),
        }
    }
}

impl<S> axum::extract::OptionalFromRequestParts<S> for ApiVersionExtractor
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        if let Some(ext) = parts.extensions.get::<ApiVersion>() {
            return Ok(Some(Self(ext.clone())));
        }
        match parse_version_from_headers(&parts.headers) {
            Ok(Some(ver)) => Ok(Some(Self(ver))),
            Ok(None) => Ok(None),
            Err(()) => Err((StatusCode::BAD_REQUEST, "invalid API version")),
        }
    }
}

fn parse_version_from_headers(headers: &HeaderMap) -> Result<Option<ApiVersion>, ()> {
    if let Some(raw) = headers.get(&X_API_VERSION) {
        let s = raw.to_str().map_err(|_| ())?;
        return ApiVersion::from_str(s).map(Some);
    }

    if let Some(accept) = headers.get(header::ACCEPT) {
        let accept_str = accept.to_str().map_err(|_| ())?;
        for media_range in accept_str.split(',').map(str::trim) {
            let media_type = media_range.split(';').next().unwrap_or("").trim();
            if let Some(rest) = media_type.strip_prefix("application/vnd.rume.") {
                let ver_str = rest.split('+').next().unwrap_or("");
                return ApiVersion::from_str(ver_str).map(Some);
            }
        }
    }

    Ok(None)
}

/// Tower layer validating `X-API-Version` or `Accept: application/vnd.rume.<ver>+json`
/// against a list of supported [`ApiVersion`]s.
///
/// Returns `400 Bad Request` for malformed headers (or missing headers in strict mode)
/// and `406 Not Acceptable` when the requested version is not in `supported`.
#[derive(Debug, Clone)]
pub struct VersionHeaderLayer {
    supported: Arc<Vec<ApiVersion>>,
    default_version: Option<ApiVersion>,
    strict: bool,
}

impl VersionHeaderLayer {
    /// Creates a [`VersionHeaderLayer`] accepting `supported` versions.
    pub fn new(supported: Vec<ApiVersion>) -> Self {
        Self {
            supported: Arc::new(supported),
            default_version: None,
            strict: false,
        }
    }

    /// Sets a default [`ApiVersion`] used when no version header is sent.
    pub fn with_default(mut self, version: ApiVersion) -> Self {
        self.default_version = Some(version);
        self
    }

    /// When `true`, requests without any version header are rejected with `400 Bad Request`.
    pub fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }
}

impl<S> Layer<S> for VersionHeaderLayer {
    type Service = VersionHeaderService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        VersionHeaderService {
            inner,
            supported: Arc::clone(&self.supported),
            default_version: self.default_version.clone(),
            strict: self.strict,
        }
    }
}

/// Tower service produced by [`VersionHeaderLayer`].
#[derive(Clone)]
pub struct VersionHeaderService<S> {
    inner: S,
    supported: Arc<Vec<ApiVersion>>,
    default_version: Option<ApiVersion>,
    strict: bool,
}

impl<S> Service<Request<Body>> for VersionHeaderService<S>
where
    S: Service<Request<Body>> + Clone + Send + 'static,
    S::Response: IntoResponse,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        let supported = Arc::clone(&self.supported);
        let default_version = self.default_version.clone();
        let strict = self.strict;

        Box::pin(async move {
            match parse_version_from_headers(req.headers()) {
                Err(()) => {
                    return Ok(
                        (StatusCode::BAD_REQUEST, "malformed API version header").into_response()
                    );
                }
                Ok(Some(ver)) => {
                    if !supported.contains(&ver) {
                        return Ok(
                            (StatusCode::NOT_ACCEPTABLE, "unsupported API version").into_response()
                        );
                    }
                    req.extensions_mut().insert(ver);
                }
                Ok(None) => {
                    if let Some(default_ver) = default_version {
                        req.extensions_mut().insert(default_ver);
                    } else if strict {
                        return Ok(
                            (StatusCode::BAD_REQUEST, "missing API version header").into_response()
                        );
                    }
                }
            }

            Ok(inner.call(req).await?.into_response())
        })
    }
}

/// Tower layer attaching RFC 8594 `Deprecation`, `Sunset`, and optional `Link`
/// (`rel="sunset"`) headers to responses.
#[derive(Debug, Clone, Default)]
pub struct DeprecationLayer {
    deprecation: Option<String>,
    sunset: Option<String>,
    sunset_link: Option<String>,
}

impl DeprecationLayer {
    /// Creates an empty [`DeprecationLayer`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `Deprecation: @<unix_secs>` per RFC 8594 §2.1.
    pub fn deprecation_timestamp(mut self, unix_secs: i64) -> Self {
        self.deprecation = Some(format!("@{unix_secs}"));
        self
    }

    /// Sets `Deprecation: true`.
    pub fn deprecated(mut self) -> Self {
        self.deprecation = Some("true".to_string());
        self
    }

    /// Sets `Sunset: <http_date>` per RFC 8594 §3.
    pub fn sunset(mut self, http_date: impl Into<String>) -> Self {
        self.sunset = Some(http_date.into());
        self
    }

    /// Sets `Link: <url>; rel="sunset"`.
    pub fn sunset_link(mut self, url: impl Into<String>) -> Self {
        self.sunset_link = Some(format!("<{}>; rel=\"sunset\"", url.into()));
        self
    }
}

impl<S> Layer<S> for DeprecationLayer {
    type Service = DeprecationService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        DeprecationService {
            inner,
            deprecation: self
                .deprecation
                .as_deref()
                .and_then(|v| HeaderValue::from_str(v).ok()),
            sunset: self
                .sunset
                .as_deref()
                .and_then(|v| HeaderValue::from_str(v).ok()),
            sunset_link: self
                .sunset_link
                .as_deref()
                .and_then(|v| HeaderValue::from_str(v).ok()),
        }
    }
}

/// Tower service produced by [`DeprecationLayer`].
#[derive(Clone)]
pub struct DeprecationService<S> {
    inner: S,
    deprecation: Option<HeaderValue>,
    sunset: Option<HeaderValue>,
    sunset_link: Option<HeaderValue>,
}

impl<S> Service<Request<Body>> for DeprecationService<S>
where
    S: Service<Request<Body>> + Clone + Send + 'static,
    S::Response: IntoResponse,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        let deprecation = self.deprecation.clone();
        let sunset = self.sunset.clone();
        let sunset_link = self.sunset_link.clone();

        Box::pin(async move {
            let mut res = inner.call(req).await?.into_response();
            let headers = res.headers_mut();
            if let Some(val) = deprecation {
                headers.insert(DEPRECATION_HEADER.clone(), val);
            }
            if let Some(val) = sunset {
                headers.insert(SUNSET_HEADER.clone(), val);
            }
            if let Some(val) = sunset_link {
                headers.append(header::LINK, val);
            }
            Ok(res)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{HeaderName, Request, StatusCode, header};
    use tower::ServiceExt;

    #[tokio::test]
    async fn versioned_routes_and_sunset_deprecation_headers_work() {
        let group = versioned_group::<()>(ApiVersion::V1)
            .get("/users", |ver: Option<ApiVersionExtractor>| async move {
                match ver {
                    Some(ApiVersionExtractor(v)) => format!("v1:{v}"),
                    None => "v1:none".to_string(),
                }
            })
            .layer(
                DeprecationLayer::new()
                    .deprecation_timestamp(1_700_000_000)
                    .sunset("Wed, 11 Nov 2026 11:11:11 GMT")
                    .sunset_link("https://docs.example.com/v2"),
            );

        let router = group
            .build()
            .into_inner()
            .layer(VersionHeaderLayer::new(vec![
                ApiVersion::V1,
                ApiVersion::V2,
            ]));

        // Valid X-API-Version: v1 -> 200 OK with Deprecation & Sunset headers.
        let ok_res = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/users")
                    .header("x-api-version", "v1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_res.status(), StatusCode::OK);
        assert_eq!(
            ok_res
                .headers()
                .get(HeaderName::from_static("deprecation"))
                .unwrap(),
            "@1700000000"
        );
        assert_eq!(
            ok_res
                .headers()
                .get(HeaderName::from_static("sunset"))
                .unwrap(),
            "Wed, 11 Nov 2026 11:11:11 GMT"
        );
        assert_eq!(
            ok_res.headers().get(header::LINK).unwrap(),
            "<https://docs.example.com/v2>; rel=\"sunset\""
        );
        let body = to_bytes(ok_res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"v1:v1");

        // Unsupported version via Accept vendor header -> 406 Not Acceptable.
        let unsupported = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/users")
                    .header(header::ACCEPT, "application/vnd.rume.v99+json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unsupported.status(), StatusCode::NOT_ACCEPTABLE);

        // Malformed X-API-Version -> 400 Bad Request.
        let malformed = router
            .oneshot(
                Request::builder()
                    .uri("/v1/users")
                    .header("x-api-version", "   ")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    }
}
