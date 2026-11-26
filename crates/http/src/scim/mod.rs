//! SCIM 2.0 HTTP routes for Rume (gated behind `scim` feature).

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rume_enterprise::enterprise::scim::{
    ScimGroup, ScimListQuery, ScimPatchRequest, ScimStore, ScimStoreError, ScimUser,
};
use serde::{Deserialize, Serialize};

/// Media type required by RFC 7644 for SCIM 2.0 responses.
pub const SCIM_JSON_MEDIA_TYPE: &str = "application/scim+json";

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

fn scim_json<T: Serialize>(status: StatusCode, value: &T, etag: Option<&str>) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => {
            let mut builder = Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, SCIM_JSON_MEDIA_TYPE);
            if let Some(tag) = etag {
                builder = builder.header(header::ETAG, tag);
            }
            builder
                .body(Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(err) => scim_error(ScimStoreError::Internal(err.to_string())),
    }
}

fn scim_error(err: ScimStoreError) -> Response {
    let status =
        StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = err.to_scim_error();
    scim_json(status, &body, None)
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
        Ok(list) => scim_json(StatusCode::OK, &list, None),
        Err(err) => scim_error(err),
    }
}

async fn create_user<S: ScimStore>(State(store): State<S>, body: Bytes) -> Response {
    let user: ScimUser = match parse_scim_body(&body) {
        Ok(u) => u,
        Err(err) => return scim_error(err),
    };
    match store.create_user(user).await {
        Ok(created) => {
            let etag = created.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::CREATED, &created, etag)
        }
        Err(err) => scim_error(err),
    }
}

async fn get_user<S: ScimStore>(State(store): State<S>, Path(id): Path<String>) -> Response {
    match store.get_user(&id).await {
        Ok(user) => {
            let etag = user.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::OK, &user, etag)
        }
        Err(err) => scim_error(err),
    }
}

async fn replace_user<S: ScimStore>(
    State(store): State<S>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let user: ScimUser = match parse_scim_body(&body) {
        Ok(u) => u,
        Err(err) => return scim_error(err),
    };
    match store.replace_user(&id, user).await {
        Ok(updated) => {
            let etag = updated.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::OK, &updated, etag)
        }
        Err(err) => scim_error(err),
    }
}

async fn patch_user<S: ScimStore>(
    State(store): State<S>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let patch: ScimPatchRequest = match parse_scim_body(&body) {
        Ok(p) => p,
        Err(err) => return scim_error(err),
    };
    match store.patch_user(&id, patch).await {
        Ok(updated) => {
            let etag = updated.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::OK, &updated, etag)
        }
        Err(err) => scim_error(err),
    }
}

async fn delete_user<S: ScimStore>(State(store): State<S>, Path(id): Path<String>) -> Response {
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
        Ok(list) => scim_json(StatusCode::OK, &list, None),
        Err(err) => scim_error(err),
    }
}

async fn create_group<S: ScimStore>(State(store): State<S>, body: Bytes) -> Response {
    let group: ScimGroup = match parse_scim_body(&body) {
        Ok(g) => g,
        Err(err) => return scim_error(err),
    };
    match store.create_group(group).await {
        Ok(created) => {
            let etag = created.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::CREATED, &created, etag)
        }
        Err(err) => scim_error(err),
    }
}

async fn get_group<S: ScimStore>(State(store): State<S>, Path(id): Path<String>) -> Response {
    match store.get_group(&id).await {
        Ok(group) => {
            let etag = group.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::OK, &group, etag)
        }
        Err(err) => scim_error(err),
    }
}

async fn replace_group<S: ScimStore>(
    State(store): State<S>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let group: ScimGroup = match parse_scim_body(&body) {
        Ok(g) => g,
        Err(err) => return scim_error(err),
    };
    match store.replace_group(&id, group).await {
        Ok(updated) => {
            let etag = updated.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::OK, &updated, etag)
        }
        Err(err) => scim_error(err),
    }
}

async fn patch_group<S: ScimStore>(
    State(store): State<S>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let patch: ScimPatchRequest = match parse_scim_body(&body) {
        Ok(p) => p,
        Err(err) => return scim_error(err),
    };
    match store.patch_group(&id, patch).await {
        Ok(updated) => {
            let etag = updated.meta.as_ref().and_then(|m| m.version.as_deref());
            scim_json(StatusCode::OK, &updated, etag)
        }
        Err(err) => scim_error(err),
    }
}

async fn delete_group<S: ScimStore>(State(store): State<S>, Path(id): Path<String>) -> Response {
    match store.delete_group(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => scim_error(err),
    }
}

/// Builds an Axum [`Router`] exposing RFC 7644 `/scim/v2/Users` and `/scim/v2/Groups` CRUD routes.
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
}
