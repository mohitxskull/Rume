use axum::routing::{get, post};
use axum::Router;
use rume::app::observability::ObservabilityLayer;
use rume::auth::policy::RequireAnyRole;
use rume::auth::{AuthLayer, AuthUser, BearerTokenScheme, PolicyLayer};
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;
use rume::tenant::resolver::HeaderTenantResolver;
use rume::tenant::{TenantLayer, TenantMembershipLayer};
use sqlx::Row;

use crate::handlers::{health, issues, organizations, projects, users};
use crate::state::WorkbenchState;

pub fn build_api_router(
    pool: DatabasePool<PostgresBackend>,
    tenant_resolver: HeaderTenantResolver,
) -> Router<WorkbenchState> {
    let auth_pool = pool.clone();
    let auth_scheme = BearerTokenScheme::new(move |token: String| {
        let pool = auth_pool.clone();
        async move {
            let sql = r#"
                SELECT id, organization_id, email, name, role
                FROM workbench_users
                WHERE api_token = $1
            "#;
            let row = sqlx::query(sql)
                .bind(&token)
                .fetch_optional(pool.backend().pool())
                .await
                .map_err(|e| rume::auth::error::AuthError::ProviderError(e.to_string()))?;

            if let Some(r) = row {
                let id: String = r
                    .try_get("id")
                    .map_err(|e| rume::auth::error::AuthError::ProviderError(e.to_string()))?;
                let org_id: String = r
                    .try_get("organization_id")
                    .map_err(|e| rume::auth::error::AuthError::ProviderError(e.to_string()))?;
                let role: String = r
                    .try_get("role")
                    .map_err(|e| rume::auth::error::AuthError::ProviderError(e.to_string()))?;
                let name: String = r
                    .try_get("name")
                    .map_err(|e| rume::auth::error::AuthError::ProviderError(e.to_string()))?;
                let email: String = r
                    .try_get("email")
                    .map_err(|e| rume::auth::error::AuthError::ProviderError(e.to_string()))?;

                let mut user = AuthUser::new(&id)
                    .with_tenant_id(&org_id)
                    .with_role(&role)
                    .with_metadata("name", name)
                    .with_metadata("email", email)
                    .with_metadata("organization_id", org_id);

                if role == "admin" || role == "member" {
                    user = user
                        .with_permission("projects:write")
                        .with_permission("issues:write");
                }
                if role == "viewer" || role == "member" || role == "admin" {
                    user = user
                        .with_permission("projects:read")
                        .with_permission("issues:read");
                }

                Ok(Some(user))
            } else {
                Ok(None)
            }
        }
    });

    let tenant_scoped_routes = Router::new()
        .route(
            "/projects",
            post(projects::create_project).get(projects::list_projects),
        )
        .route("/projects/{id}", get(projects::get_project))
        .route(
            "/projects/{project_id}/issues",
            post(issues::create_issue).get(issues::list_issues),
        )
        .route(
            "/issues/{id}",
            get(issues::get_issue).patch(issues::update_issue),
        )
        .route("/users", post(users::create_user).get(users::list_users))
        .route("/users/me", get(users::get_current_user))
        .route("/users/{id}", get(users::get_user))
        .layer(PolicyLayer::new(RequireAnyRole(&["admin", "member"])))
        .layer(TenantMembershipLayer::strict())
        .layer(AuthLayer::required().scheme(auth_scheme))
        .layer(TenantLayer::required(tenant_resolver));

    let public_org_routes = Router::new()
        .route(
            "/organizations",
            post(organizations::create_organization).get(organizations::list_organizations),
        )
        .route(
            "/organizations/{id}",
            get(organizations::get_organization),
        );

    Router::new()
        .nest("/api/v1", tenant_scoped_routes)
        .nest("/api/v1", public_org_routes)
        .route("/health", get(health::health))
        .layer(ObservabilityLayer::new())
}
