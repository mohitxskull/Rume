use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use rume::auth::Authenticated;
use rume::auth::identity::AuthUser;
use rume::error::AppError;
use rume::tenant::TenantContext;

use crate::models::{CreateUserRequest, User};
use crate::repositories::UserRepository;
use crate::state::WorkbenchState;

pub(crate) fn validate_create_user_request<'a>(
    caller: &AuthUser,
    payload: &'a CreateUserRequest,
) -> Result<(&'a str, &'a str, &'a str, &'a str, &'a str), AppError> {
    // Why: only organization administrators may provision new users and assign roles/API tokens;
    // allowing ordinary `member` accounts to call `POST /api/v1/users` would let any member mint
    // an `admin` account with a caller-chosen token and escalate privileges within the tenant.
    if !caller.has_role("admin") {
        return Err(AppError::Forbidden);
    }

    let id = payload.id.trim();
    let email = payload.email.trim();
    let name = payload.name.trim();
    let role = payload.role.trim();
    let token = payload.api_token.trim();

    if id.is_empty() || email.is_empty() || name.is_empty() || role.is_empty() || token.is_empty() {
        return Err(AppError::bad_request("All user fields are required"));
    }

    if !matches!(role, "admin" | "member" | "viewer") {
        return Err(AppError::bad_request(
            "Role must be one of: admin, member, viewer",
        ));
    }

    Ok((id, email, name, role, token))
}

pub async fn create_user(
    Authenticated(caller): Authenticated<AuthUser>,
    TenantContext(tenant): TenantContext,
    State(state): State<WorkbenchState>,
    Json(payload): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<User>), AppError> {
    let (id, email, name, role, token) = validate_create_user_request(&caller, &payload)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    let user = User {
        id: id.to_string(),
        organization_id: tenant.id.clone(),
        email: email.to_string(),
        name: name.to_string(),
        role: role.to_string(),
        api_token: token.to_string(),
        created_at: now,
    };

    UserRepository::create(&state.pool, &user)
        .await
        .map_err(|e| AppError::internal(format!("Failed to create user: {e}")))?;

    Ok((StatusCode::CREATED, Json(user)))
}

pub async fn list_users(
    TenantContext(tenant): TenantContext,
    State(state): State<WorkbenchState>,
) -> Result<Json<Vec<User>>, AppError> {
    let users = UserRepository::list_by_organization(&state.pool, &tenant.id)
        .await
        .map_err(|e| AppError::internal(format!("Database error: {e}")))?;

    Ok(Json(users))
}

pub async fn get_user(
    TenantContext(tenant): TenantContext,
    Path(id): Path<String>,
    State(state): State<WorkbenchState>,
) -> Result<Json<User>, AppError> {
    let user = UserRepository::find(&state.pool, &id, &tenant.id)
        .await
        .map_err(|e| AppError::internal(format!("Database error: {e}")))?
        .ok_or_else(|| AppError::not_found(format!("User '{id}' not found in organization")))?;

    Ok(Json(user))
}

pub async fn get_current_user(
    Authenticated(user): Authenticated<AuthUser>,
) -> Result<Json<AuthUser>, AppError> {
    Ok(Json(user))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::post;
    use rume::auth::policy::RequireAnyRole;
    use rume::auth::{AuthLayer, BearerTokenScheme, PolicyLayer};
    use rume::tenant::Tenant;
    use tower::ServiceExt;

    #[tokio::test]
    async fn member_cannot_call_create_user_to_escalate_to_admin() {
        let auth_scheme = BearerTokenScheme::new(|token: String| async move {
            match token.as_str() {
                "member-token" => Ok(Some(
                    AuthUser::new("user-member")
                        .with_tenant_id("acme-corp")
                        .with_role("member"),
                )),
                "admin-token" => Ok(Some(
                    AuthUser::new("user-admin")
                        .with_tenant_id("acme-corp")
                        .with_role("admin"),
                )),
                _ => Ok(None),
            }
        });

        async fn production_validation_endpoint(
            Authenticated(caller): Authenticated<AuthUser>,
            TenantContext(_tenant): TenantContext,
            Json(payload): Json<CreateUserRequest>,
        ) -> Result<StatusCode, AppError> {
            validate_create_user_request(&caller, &payload)?;
            Ok(StatusCode::CREATED)
        }

        // Mirrors the route-level `RequireAnyRole(&["admin", "member"])` policy from `routes.rs`
        // so that authorization and role-whitelist enforcement are exercised through the shared
        // production `validate_create_user_request` helper used by `create_user`.
        let app = Router::new()
            .route(
                "/users",
                post(production_validation_endpoint)
                    .layer(PolicyLayer::new(RequireAnyRole(&["admin", "member"]))),
            )
            .layer(AuthLayer::required().scheme(auth_scheme));

        // 1. Member attempting to create an admin account is rejected with 403 Forbidden
        let mut member_req = Request::builder()
            .method("POST")
            .uri("/users")
            .header("authorization", "Bearer member-token")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"id":"u-esc","email":"e@acme.com","name":"Esc","role":"admin","api_token":"tok-1"}"#,
            ))
            .unwrap();
        member_req
            .extensions_mut()
            .insert(Tenant::new("acme-corp", "acme-corp"));

        let member_res = app.clone().oneshot(member_req).await.unwrap();
        assert_eq!(member_res.status(), StatusCode::FORBIDDEN);

        // 2. Admin creating a user with an invalid role is rejected with 400 Bad Request
        let mut bad_role_req = Request::builder()
            .method("POST")
            .uri("/users")
            .header("authorization", "Bearer admin-token")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"id":"u-bad","email":"b@acme.com","name":"Bad","role":"superroot","api_token":"tok-2"}"#,
            ))
            .unwrap();
        bad_role_req
            .extensions_mut()
            .insert(Tenant::new("acme-corp", "acme-corp"));

        let bad_role_res = app.clone().oneshot(bad_role_req).await.unwrap();
        assert_eq!(bad_role_res.status(), StatusCode::BAD_REQUEST);

        // 3. Admin creating a valid member account succeeds with 201 Created
        let mut admin_req = Request::builder()
            .method("POST")
            .uri("/users")
            .header("authorization", "Bearer admin-token")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"id":"u-ok","email":"ok@acme.com","name":"Ok","role":"member","api_token":"tok-3"}"#,
            ))
            .unwrap();
        admin_req
            .extensions_mut()
            .insert(Tenant::new("acme-corp", "acme-corp"));

        let admin_res = app.oneshot(admin_req).await.unwrap();
        assert_eq!(admin_res.status(), StatusCode::CREATED);
    }
}
