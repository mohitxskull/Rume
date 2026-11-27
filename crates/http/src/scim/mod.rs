//! SCIM 2.0 HTTP routes for Rume (gated behind `scim` feature).

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rume_enterprise::enterprise::scim::{
    ScimErrorResponse, ScimGroup, ScimListQuery, ScimPatchRequest, ScimStore, ScimStoreError,
    ScimUser,
};
use serde::{Deserialize, Serialize};

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

fn if_match_satisfies(header_val: &str, current_etag: Option<&str>) -> bool {
    let Some(current) = current_etag else {
        return header_val.trim() == "*";
    };
    let current_norm = current.trim().strip_prefix("W/").unwrap_or(current.trim());
    header_val.split(',').map(str::trim).any(|candidate| {
        if candidate == "*" {
            return true;
        }
        let cand_norm = candidate.strip_prefix("W/").unwrap_or(candidate);
        cand_norm == current_norm
    })
}

fn check_if_match_header(headers: &HeaderMap, current_etag: Option<&str>) -> Option<Response> {
    let raw = headers.get(header::IF_MATCH)?;
    let Ok(if_match) = raw.to_str() else {
        return Some(scim_status_error(
            StatusCode::PRECONDITION_FAILED,
            None,
            "invalid If-Match header",
        ));
    };
    if if_match_satisfies(if_match, current_etag) {
        None
    } else {
        Some(scim_status_error(
            StatusCode::PRECONDITION_FAILED,
            None,
            "resource version does not match If-Match precondition",
        ))
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
            let location = format!("/scim/v2/Users/{}", created.id);
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
    if headers.contains_key(header::IF_MATCH) {
        match store.get_user(&id).await {
            Ok(existing) => {
                let current_etag = existing.meta.as_ref().and_then(|m| m.version.as_deref());
                if let Some(res) = check_if_match_header(&headers, current_etag) {
                    return res;
                }
            }
            Err(err) => return scim_error(err),
        }
    }
    let user: ScimUser = match parse_scim_body(&body) {
        Ok(u) => u,
        Err(err) => return scim_error(err),
    };
    match store.replace_user(&id, user).await {
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
    if headers.contains_key(header::IF_MATCH) {
        match store.get_user(&id).await {
            Ok(existing) => {
                let current_etag = existing.meta.as_ref().and_then(|m| m.version.as_deref());
                if let Some(res) = check_if_match_header(&headers, current_etag) {
                    return res;
                }
            }
            Err(err) => return scim_error(err),
        }
    }
    let patch: ScimPatchRequest = match parse_scim_body(&body) {
        Ok(p) => p,
        Err(err) => return scim_error(err),
    };
    match store.patch_user(&id, patch).await {
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
    if headers.contains_key(header::IF_MATCH) {
        match store.get_user(&id).await {
            Ok(existing) => {
                let current_etag = existing.meta.as_ref().and_then(|m| m.version.as_deref());
                if let Some(res) = check_if_match_header(&headers, current_etag) {
                    return res;
                }
            }
            Err(err) => return scim_error(err),
        }
    }
    match store.delete_user(&id).await {
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
            let location = format!("/scim/v2/Groups/{}", created.id);
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
    if headers.contains_key(header::IF_MATCH) {
        match store.get_group(&id).await {
            Ok(existing) => {
                let current_etag = existing.meta.as_ref().and_then(|m| m.version.as_deref());
                if let Some(res) = check_if_match_header(&headers, current_etag) {
                    return res;
                }
            }
            Err(err) => return scim_error(err),
        }
    }
    let group: ScimGroup = match parse_scim_body(&body) {
        Ok(g) => g,
        Err(err) => return scim_error(err),
    };
    match store.replace_group(&id, group).await {
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
    if headers.contains_key(header::IF_MATCH) {
        match store.get_group(&id).await {
            Ok(existing) => {
                let current_etag = existing.meta.as_ref().and_then(|m| m.version.as_deref());
                if let Some(res) = check_if_match_header(&headers, current_etag) {
                    return res;
                }
            }
            Err(err) => return scim_error(err),
        }
    }
    let patch: ScimPatchRequest = match parse_scim_body(&body) {
        Ok(p) => p,
        Err(err) => return scim_error(err),
    };
    match store.patch_group(&id, patch).await {
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
    if headers.contains_key(header::IF_MATCH) {
        match store.get_group(&id).await {
            Ok(existing) => {
                let current_etag = existing.meta.as_ref().and_then(|m| m.version.as_deref());
                if let Some(res) = check_if_match_header(&headers, current_etag) {
                    return res;
                }
            }
            Err(err) => return scim_error(err),
        }
    }
    match store.delete_group(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => scim_error(err),
    }
}

/// Builds an Axum [`Router`] exposing RFC 7644 `/scim/v2/Users` and `/scim/v2/Groups` CRUD routes.
///
/// # Security
///
/// This router does **not** enforce authentication or authorization by itself.
/// Callers **must** wrap it in authentication and authorization middleware
/// (for example, a bearer-token / OAuth2 authenticator plus SCIM admin scope check)
/// before exposing it on any network interface.
pub fn scim_routes<S>(store: S) -> Router
where
    S: ScimStore + Clone + Send + Sync + 'static,
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

    #[tokio::test]
    async fn scim_http_users_and_groups_crud_and_filter_roundtrip() {
        let store = InMemoryScimStore::new();
        let app = scim_routes(store);

        // 1. Create User
        let user_payload = ScimUser::new("", "alice").with_email("alice@example.com", true);
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/scim/v2/Users")
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
        let app = scim_routes(store);

        // 1. Invalid Content-Type -> 415 Unsupported Media Type
        let user_payload = ScimUser::new("", "bob");
        let res_bad_ct = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/scim/v2/Users")
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
                    .header(header::IF_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_del.status(), StatusCode::NO_CONTENT);
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
