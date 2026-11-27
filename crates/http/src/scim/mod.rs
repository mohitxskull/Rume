//! SCIM 2.0 HTTP routes for Rume (gated behind `scim` feature).

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{Route, get};
use rume_enterprise::enterprise::scim::{
    ScimErrorResponse, ScimGroup, ScimListQuery, ScimPatchRequest, ScimStore, ScimStoreError,
    ScimUser,
};
use serde::{Deserialize, Serialize};
use tower::{Layer, Service};

/// Media type required by RFC 7644 for SCIM 2.0 responses.
pub const SCIM_JSON_MEDIA_TYPE: &str = "application/scim+json";

/// Static RFC 7644 error fallback used if serializing a SCIM response fails.
const SCIM_SERIALIZATION_FALLBACK: &str = r#"{"schemas":["urn:ietf:params:scim:api:messages:2.0:Error"],"detail":"failed to serialize SCIM response","status":"500"}"#;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListParams {
    filter: Option<String>,
    start_index: Option<usize>,
    count: Option<usize>,
}

impl From<ListParams> for ScimListQuery {
    fn from(p: ListParams) -> Self {
        Self {
            filter: p.filter,
            start_index: p.start_index.unwrap_or(1),
            count: p.count,
        }
    }
}

fn scim_json<T: Serialize>(
    status: StatusCode,
    value: &T,
    etag: Option<&str>,
    location: Option<&str>,
) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => {
            let mut builder = Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, SCIM_JSON_MEDIA_TYPE);
            if let Some(tag) = etag
                && let Ok(val) = HeaderValue::from_str(tag)
            {
                builder = builder.header(header::ETAG, val);
            }
            if let Some(loc) = location
                && let Ok(val) = HeaderValue::from_str(loc)
            {
                builder = builder.header(header::LOCATION, val);
            }
            builder
                .body(Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(_) => {
            // Why: return a static JSON 500 fallback directly instead of calling `scim_error`
            // -> `scim_json`, breaking potential infinite recursion on serialization failure (`RC-26`).
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, SCIM_JSON_MEDIA_TYPE)
                .body(Body::from(SCIM_SERIALIZATION_FALLBACK))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

fn scim_error(err: ScimStoreError) -> Response {
    let status =
        StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = err.to_scim_error();
    scim_json(status, &body, None, None)
}

fn scim_status_error(status: StatusCode, scim_type: Option<&str>, detail: &str) -> Response {
    let body = ScimErrorResponse::new(status.as_u16(), scim_type, Some(detail));
    scim_json(status, &body, None, None)
}

fn validate_scim_content_type(headers: &HeaderMap) -> Option<Response> {
    let Some(raw) = headers.get(header::CONTENT_TYPE) else {
        return Some(scim_status_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            None,
            "missing Content-Type header; expected application/scim+json or application/json",
        ));
    };
    let Ok(ct) = raw.to_str() else {
        return Some(scim_status_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            None,
            "invalid Content-Type header",
        ));
    };
    let essence = ct.split(';').next().unwrap_or("").trim();
    if essence.eq_ignore_ascii_case(SCIM_JSON_MEDIA_TYPE)
        || essence.eq_ignore_ascii_case("application/json")
    {
        None
    } else {
        Some(scim_status_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            None,
            &format!(
                "unsupported Content-Type '{essence}'; expected application/scim+json or application/json"
            ),
        ))
    }
}

/// Folds all `If-Match` request headers into a single RFC 9110 comma-separated string so
/// downstream atomic CAS in [`ScimStore`] sees every header line (`P2-08`).
fn extract_if_match_header(headers: &HeaderMap) -> Result<Option<String>, ScimStoreError> {
    let mut values = Vec::new();
    for raw in headers.get_all(header::IF_MATCH) {
        let Ok(s) = raw.to_str() else {
            return Err(ScimStoreError::PreconditionFailed {
                detail: "invalid If-Match header".into(),
            });
        };
        values.push(s.trim());
    }
    if values.is_empty() {
        Ok(None)
    } else {
        Ok(Some(values.join(", ")))
    }
}

/// Percent-encodes a single URI path segment per RFC 3986 (leaving only unreserved bytes
/// `ALPHA / DIGIT / "-" / "." / "_" / "~"` unencoded).
fn percent_encode_path_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    out
}

/// Builds the RFC 7644 `Location` header for a newly created resource, preserving any
/// mount prefix and scheme/authority present on `original_uri` and percent-encoding `id`.
fn build_created_location(original_uri: &Uri, id: &str) -> String {
    let base_path = original_uri.path().trim_end_matches('/');
    let encoded_id = percent_encode_path_segment(id);
    if let (Some(scheme), Some(authority)) = (original_uri.scheme_str(), original_uri.authority()) {
        format!("{scheme}://{authority}{base_path}/{encoded_id}")
    } else {
        format!("{base_path}/{encoded_id}")
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Bearer-token authentication [`Layer`] for [`scim_routes`], validating
/// `Authorization: Bearer <token>` in constant time and returning RFC 7644 `401 Unauthorized`
/// responses on missing or invalid credentials.
#[derive(Clone)]
pub struct ScimBearerAuthLayer {
    token: Arc<str>,
}

impl std::fmt::Debug for ScimBearerAuthLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScimBearerAuthLayer")
            .field("token", &"[REDACTED]")
            .finish()
    }
}

impl ScimBearerAuthLayer {
    /// Creates a new bearer-token auth layer requiring `Authorization: Bearer <token>`.
    ///
    /// # Panics
    ///
    /// Panics if `token` is empty or whitespace-only.
    pub fn new(token: impl Into<String>) -> Self {
        let token = token.into();
        assert!(
            !token.trim().is_empty(),
            "ScimBearerAuthLayer token must be non-empty"
        );
        Self {
            token: Arc::from(token),
        }
    }
}

impl<S> Layer<S> for ScimBearerAuthLayer {
    type Service = ScimBearerAuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ScimBearerAuthService {
            inner,
            token: Arc::clone(&self.token),
        }
    }
}

/// Tower [`Service`] produced by [`ScimBearerAuthLayer`].
#[derive(Clone)]
pub struct ScimBearerAuthService<S> {
    inner: S,
    token: Arc<str>,
}

impl<S> Service<Request<Body>> for ScimBearerAuthService<S>
where
    S: Service<Request<Body>, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let mut auth_headers = req.headers().get_all(header::AUTHORIZATION).iter();
        let authorized = match (auth_headers.next(), auth_headers.next()) {
            (Some(val), None) => val
                .to_str()
                .ok()
                .and_then(|s| s.split_once(' '))
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                .map(|(_, token)| token.trim())
                .filter(|token| !token.is_empty())
                .is_some_and(|presented| {
                    constant_time_eq(presented.as_bytes(), self.token.as_bytes())
                }),
            _ => false,
        };

        if !authorized {
            let mut res = scim_status_error(
                StatusCode::UNAUTHORIZED,
                None,
                "missing or invalid SCIM bearer token",
            );
            res.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"scim\""),
            );
            return Box::pin(async move { Ok(res) });
        }

        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move { inner.call(req).await })
    }
}

fn parse_scim_body<T: serde::de::DeserializeOwned>(bytes: &Bytes) -> Result<T, ScimStoreError> {
    serde_json::from_slice(bytes).map_err(|err| ScimStoreError::BadRequest {
        scim_type: Some("invalidSyntax"),
        detail: err.to_string(),
    })
}

async fn list_users<S: ScimStore>(
    State(store): State<S>,
    Query(params): Query<ListParams>,
) -> Response {
    match store.list_users(params.into()).await {
        Ok(list) => scim_json(StatusCode::OK, &list, None, None),
        Err(err) => scim_error(err),
    }
}

async fn create_user<S: ScimStore>(
    State(store): State<S>,
    OriginalUri(original_uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(res) = validate_scim_content_type(&headers) {
        return res;
    }
    let user: ScimUser = match parse_scim_body(&body) {
        Ok(u) => u,
        Err(err) => return scim_error(err),
    };
    match store.create_user(user).await {
        Ok(created) => {
            let etag = created.meta.as_ref().and_then(|m| m.version.as_deref());
            let location = build_created_location(&original_uri, &created.id);
            scim_json(StatusCode::CREATED, &created, etag, Some(&location))
        }
        Err(err) => scim_error(err),
    }
}

async fn get_user<S: ScimStore>(State(store): State<S>, Path(id): Path<String>) -> Response {
    match store.get_user(&id).await {
        Ok(user) => {
            let etag = user.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::OK, &user, etag, None)
        }
        Err(err) => scim_error(err),
    }
}

async fn replace_user<S: ScimStore>(
    State(store): State<S>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(res) = validate_scim_content_type(&headers) {
        return res;
    }
    let expected_version = match extract_if_match_header(&headers) {
        Ok(v) => v,
        Err(err) => return scim_error(err),
    };
    let user: ScimUser = match parse_scim_body(&body) {
        Ok(u) => u,
        Err(err) => return scim_error(err),
    };
    match store
        .replace_user(&id, user, expected_version.as_deref())
        .await
    {
        Ok(updated) => {
            let etag = updated.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::OK, &updated, etag, None)
        }
        Err(err) => scim_error(err),
    }
}

async fn patch_user<S: ScimStore>(
    State(store): State<S>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(res) = validate_scim_content_type(&headers) {
        return res;
    }
    let expected_version = match extract_if_match_header(&headers) {
        Ok(v) => v,
        Err(err) => return scim_error(err),
    };
    let patch: ScimPatchRequest = match parse_scim_body(&body) {
        Ok(p) => p,
        Err(err) => return scim_error(err),
    };
    match store
        .patch_user(&id, patch, expected_version.as_deref())
        .await
    {
        Ok(updated) => {
            let etag = updated.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::OK, &updated, etag, None)
        }
        Err(err) => scim_error(err),
    }
}

async fn delete_user<S: ScimStore>(
    State(store): State<S>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let expected_version = match extract_if_match_header(&headers) {
        Ok(v) => v,
        Err(err) => return scim_error(err),
    };
    match store.delete_user(&id, expected_version.as_deref()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => scim_error(err),
    }
}

async fn list_groups<S: ScimStore>(
    State(store): State<S>,
    Query(params): Query<ListParams>,
) -> Response {
    match store.list_groups(params.into()).await {
        Ok(list) => scim_json(StatusCode::OK, &list, None, None),
        Err(err) => scim_error(err),
    }
}

async fn create_group<S: ScimStore>(
    State(store): State<S>,
    OriginalUri(original_uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(res) = validate_scim_content_type(&headers) {
        return res;
    }
    let group: ScimGroup = match parse_scim_body(&body) {
        Ok(g) => g,
        Err(err) => return scim_error(err),
    };
    match store.create_group(group).await {
        Ok(created) => {
            let etag = created.meta.as_ref().and_then(|m| m.version.as_deref());
            let location = build_created_location(&original_uri, &created.id);
            scim_json(StatusCode::CREATED, &created, etag, Some(&location))
        }
        Err(err) => scim_error(err),
    }
}

async fn get_group<S: ScimStore>(State(store): State<S>, Path(id): Path<String>) -> Response {
    match store.get_group(&id).await {
        Ok(group) => {
            let etag = group.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::OK, &group, etag, None)
        }
        Err(err) => scim_error(err),
    }
}

async fn replace_group<S: ScimStore>(
    State(store): State<S>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(res) = validate_scim_content_type(&headers) {
        return res;
    }
    let expected_version = match extract_if_match_header(&headers) {
        Ok(v) => v,
        Err(err) => return scim_error(err),
    };
    let group: ScimGroup = match parse_scim_body(&body) {
        Ok(g) => g,
        Err(err) => return scim_error(err),
    };
    match store
        .replace_group(&id, group, expected_version.as_deref())
        .await
    {
        Ok(updated) => {
            let etag = updated.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::OK, &updated, etag, None)
        }
        Err(err) => scim_error(err),
    }
}

async fn patch_group<S: ScimStore>(
    State(store): State<S>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(res) = validate_scim_content_type(&headers) {
        return res;
    }
    let expected_version = match extract_if_match_header(&headers) {
        Ok(v) => v,
        Err(err) => return scim_error(err),
    };
    let patch: ScimPatchRequest = match parse_scim_body(&body) {
        Ok(p) => p,
        Err(err) => return scim_error(err),
    };
    match store
        .patch_group(&id, patch, expected_version.as_deref())
        .await
    {
        Ok(updated) => {
            let etag = updated.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::OK, &updated, etag, None)
        }
        Err(err) => scim_error(err),
    }
}

async fn delete_group<S: ScimStore>(
    State(store): State<S>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let expected_version = match extract_if_match_header(&headers) {
        Ok(v) => v,
        Err(err) => return scim_error(err),
    };
    match store.delete_group(&id, expected_version.as_deref()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => scim_error(err),
    }
}

/// Builds an Axum [`Router`] exposing RFC 7644 `/scim/v2/Users` and `/scim/v2/Groups` CRUD routes,
/// protected by the required `auth_layer` middleware (e.g. [`ScimBearerAuthLayer`]).
pub fn scim_routes<S, L>(store: S, auth_layer: L) -> Router
where
    S: ScimStore + Clone + Send + Sync + 'static,
    L: Layer<Route> + Clone + Send + Sync + 'static,
    L::Service: Service<Request<Body>> + Clone + Send + Sync + 'static,
    <L::Service as Service<Request<Body>>>::Response: IntoResponse + 'static,
    <L::Service as Service<Request<Body>>>::Error: Into<Infallible> + 'static,
    <L::Service as Service<Request<Body>>>::Future: Send + 'static,
{
    Router::new()
        .route(
            "/scim/v2/Users",
            get(list_users::<S>).post(create_user::<S>),
        )
        .route(
            "/scim/v2/Users/{id}",
            get(get_user::<S>)
                .put(replace_user::<S>)
                .patch(patch_user::<S>)
                .delete(delete_user::<S>),
        )
        .route(
            "/scim/v2/Groups",
            get(list_groups::<S>).post(create_group::<S>),
        )
        .route(
            "/scim/v2/Groups/{id}",
            get(get_group::<S>)
                .put(replace_group::<S>)
                .patch(patch_group::<S>)
                .delete(delete_group::<S>),
        )
        .layer(auth_layer)
        .with_state(store)
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use rume_enterprise::enterprise::scim::{
        InMemoryScimStore, SCIM_GROUP_SCHEMA, SCIM_USER_SCHEMA, ScimGroup, ScimListResponse,
        ScimUser,
    };
    use tower::ServiceExt;

    use super::*;

    const TEST_TOKEN: &str = "test-scim-secret";
    const TEST_AUTH: &str = "Bearer test-scim-secret";

    #[tokio::test]
    async fn scim_http_users_and_groups_crud_and_filter_roundtrip() {
        let store = InMemoryScimStore::new();
        let app = scim_routes(store, ScimBearerAuthLayer::new(TEST_TOKEN));

        // 1. Create User
        let user_payload = ScimUser::new("", "alice").with_email("alice@example.com", true);
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/scim/v2/Users")
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .header(header::CONTENT_TYPE, SCIM_JSON_MEDIA_TYPE)
                    .body(Body::from(serde_json::to_vec(&user_payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some(SCIM_JSON_MEDIA_TYPE)
        );
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let created_user: ScimUser = serde_json::from_slice(&body).unwrap();
        assert!(!created_user.id.is_empty());
        assert_eq!(created_user.user_name, "alice");
        assert!(created_user.schemas.contains(&SCIM_USER_SCHEMA.to_string()));

        // 2. List Users with filter + pagination
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/scim/v2/Users?filter=userName%20eq%20%22alice%22&startIndex=1&count=10")
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let list: ScimListResponse<ScimUser> = serde_json::from_slice(&body).unwrap();
        assert_eq!(list.total_results, 1);
        assert_eq!(list.start_index, 1);
        assert_eq!(list.resources[0].id, created_user.id);

        // 3. Patch User
        let patch_body = serde_json::json!({
            "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
            "Operations": [
                { "op": "replace", "path": "active", "value": false }
            ]
        });
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/scim/v2/Users/{}", created_user.id))
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .header(header::CONTENT_TYPE, SCIM_JSON_MEDIA_TYPE)
                    .body(Body::from(serde_json::to_vec(&patch_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let patched_user: ScimUser = serde_json::from_slice(&body).unwrap();
        assert!(!patched_user.active);

        // 4. Replace User (PUT)
        let mut replacement = patched_user.clone();
        replacement.display_name = Some("Alice Replaced".to_string());
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/scim/v2/Users/{}", created_user.id))
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .header(header::CONTENT_TYPE, SCIM_JSON_MEDIA_TYPE)
                    .body(Body::from(serde_json::to_vec(&replacement).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let replaced_user: ScimUser = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            replaced_user.display_name.as_deref(),
            Some("Alice Replaced")
        );

        // 5. Delete User -> 204, then GET -> 404
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/scim/v2/Users/{}", created_user.id))
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/scim/v2/Users/{}", created_user.id))
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        // 6. Group CRUD roundtrip
        let group_payload = ScimGroup::new("", "Engineering");
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/scim/v2/Groups")
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .header(header::CONTENT_TYPE, SCIM_JSON_MEDIA_TYPE)
                    .body(Body::from(serde_json::to_vec(&group_payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let created_group: ScimGroup = serde_json::from_slice(&body).unwrap();
        assert!(!created_group.id.is_empty());
        assert!(
            created_group
                .schemas
                .contains(&SCIM_GROUP_SCHEMA.to_string())
        );

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/scim/v2/Groups?filter=displayName%20eq%20%22Engineering%22&startIndex=1&count=10")
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let group_list: ScimListResponse<ScimGroup> = serde_json::from_slice(&body).unwrap();
        assert_eq!(group_list.total_results, 1);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/scim/v2/Groups/{}", created_group.id))
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);

        let res = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/scim/v2/Groups/{}", created_group.id))
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn scim_http_enforces_content_type_location_and_if_match() {
        let store = InMemoryScimStore::new();
        let app = scim_routes(store, ScimBearerAuthLayer::new(TEST_TOKEN));

        // 1. Invalid Content-Type -> 415 Unsupported Media Type
        let user_payload = ScimUser::new("", "bob");
        let res_bad_ct = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/scim/v2/Users")
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from(serde_json::to_vec(&user_payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_bad_ct.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        // 2. Valid Content-Type with charset parameter -> 201 Created + Location header
        let res_created = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/scim/v2/Users")
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
                    .body(Body::from(serde_json::to_vec(&user_payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_created.status(), StatusCode::CREATED);
        let location = res_created
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let etag = res_created
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_string();
        let body = to_bytes(res_created.into_body(), usize::MAX).await.unwrap();
        let created: ScimUser = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            location.as_deref(),
            Some(format!("/scim/v2/Users/{}", created.id).as_str())
        );

        // 3. Stale If-Match on PATCH -> 412 Precondition Failed
        let patch_body = serde_json::json!({
            "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
            "Operations": [{ "op": "replace", "path": "active", "value": false }]
        });
        let res_stale = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/scim/v2/Users/{}", created.id))
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .header(header::CONTENT_TYPE, SCIM_JSON_MEDIA_TYPE)
                    .header(header::IF_MATCH, "W/\"v999\"")
                    .body(Body::from(serde_json::to_vec(&patch_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_stale.status(), StatusCode::PRECONDITION_FAILED);

        // 4. Matching If-Match on DELETE -> 204 No Content
        let res_del = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/scim/v2/Users/{}", created.id))
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .header(header::IF_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_del.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn scim_p2_08_auth_multi_header_if_match_concurrent_cas_and_percent_encoded_location() {
        let store = InMemoryScimStore::new();
        let app = scim_routes(store.clone(), ScimBearerAuthLayer::new(TEST_TOKEN));

        // 1. Auth enforcement: missing or wrong bearer token -> 401 Unauthorized.
        let unauth = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/scim/v2/Users")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);
        assert!(unauth.headers().contains_key(header::WWW_AUTHENTICATE));

        let wrong_tok = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/scim/v2/Users")
                    .header(header::AUTHORIZATION, "Bearer wrong-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong_tok.status(), StatusCode::UNAUTHORIZED);

        // 2. Percent-encoded Location + preserved mount prefix under `.nest("/tenant/acme", ...)`.
        let nested = Router::new().nest(
            "/tenant/acme",
            scim_routes(store.clone(), ScimBearerAuthLayer::new(TEST_TOKEN)),
        );
        let special_user = ScimUser::new("user/a b?c#d", "charlie");
        let res_created = nested
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tenant/acme/scim/v2/Users")
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .header(header::CONTENT_TYPE, SCIM_JSON_MEDIA_TYPE)
                    .body(Body::from(serde_json::to_vec(&special_user).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_created.status(), StatusCode::CREATED);
        assert_eq!(
            res_created
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some("/tenant/acme/scim/v2/Users/user%2Fa%20b%3Fc%23d")
        );

        // 3. Multi-header `If-Match` folding: second header line carries the matching ETag.
        let dave = ScimUser::new("", "dave");
        let res_dave = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/scim/v2/Users")
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .header(header::CONTENT_TYPE, SCIM_JSON_MEDIA_TYPE)
                    .body(Body::from(serde_json::to_vec(&dave).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let etag_v1 = res_dave
            .headers()
            .get(header::ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let created_dave: ScimUser =
            serde_json::from_slice(&to_bytes(res_dave.into_body(), usize::MAX).await.unwrap())
                .unwrap();

        let patch_body = serde_json::json!({
            "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
            "Operations": [{ "op": "replace", "path": "active", "value": false }]
        });
        let res_multi_if_match = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/scim/v2/Users/{}", created_dave.id))
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .header(header::CONTENT_TYPE, SCIM_JSON_MEDIA_TYPE)
                    .header(header::IF_MATCH, "\"other,with,comma\"")
                    .header(header::IF_MATCH, etag_v1.as_str())
                    .body(Body::from(serde_json::to_vec(&patch_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_multi_if_match.status(), StatusCode::OK);

        // 4. Stale `etag_v1` now fails atomically with 412 Precondition Failed.
        let mut repl = created_dave.clone();
        repl.display_name = Some("Stale Update".to_string());
        let res_conflict = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/scim/v2/Users/{}", created_dave.id))
                    .header(header::AUTHORIZATION, TEST_AUTH)
                    .header(header::CONTENT_TYPE, SCIM_JSON_MEDIA_TYPE)
                    .header(header::IF_MATCH, etag_v1)
                    .body(Body::from(serde_json::to_vec(&repl).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_conflict.status(), StatusCode::PRECONDITION_FAILED);

        // 5. Absolute URI with scheme + authority is preserved in Location.
        let abs_uri: Uri = "https://api.example.com/scim/v2/Users".parse().unwrap();
        assert_eq!(
            build_created_location(&abs_uri, "id with space"),
            "https://api.example.com/scim/v2/Users/id%20with%20space"
        );
    }

    #[test]
    fn scim_json_handles_invalid_etag_and_serialization_failure_without_recursion() {
        struct FailSerialize;
        impl Serialize for FailSerialize {
            fn serialize<S: serde::Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("boom"))
            }
        }

        let res_fail = scim_json(StatusCode::OK, &FailSerialize, None, None);
        assert_eq!(res_fail.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let res_bad_etag = scim_json(
            StatusCode::OK,
            &serde_json::json!({"ok": true}),
            Some("bad\r\netag"),
            None,
        );
        assert_eq!(res_bad_etag.status(), StatusCode::OK);
        assert!(res_bad_etag.headers().get(header::ETAG).is_none());
    }
}
