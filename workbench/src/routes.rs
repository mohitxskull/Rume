use axum::routing::{get, post};
use axum::Router;
use rume::app::observability::ObservabilityLayer;
use rume::auth::policy::RequireAnyRole;
use rume::auth::{AuthLayer, AuthUser, BearerTokenScheme, PolicyLayer};
use rume::tenant::resolver::HeaderTenantResolver;
use rume::tenant::TenantLayer;

use crate::handlers::{issues, projects};
use crate::state::WorkbenchState;

pub fn build_api_router(tenant_resolver: HeaderTenantResolver) -> Router<WorkbenchState> {
    let auth_scheme = BearerTokenScheme::new(|token: String| async move {
        match token.as_str() {
            "admin-token" => Ok(Some(
                AuthUser::new("user-admin")
                    .with_role("admin")
                    .with_role("member")
                    .with_permission("projects:write")
                    .with_permission("issues:write"),
            )),
            "member-token" => Ok(Some(
                AuthUser::new("user-member")
                    .with_role("member")
                    .with_permission("projects:write")
                    .with_permission("issues:write"),
            )),
            "viewer-token" => Ok(Some(
                AuthUser::new("user-viewer")
                    .with_role("viewer")
                    .with_permission("projects:read"),
            )),
            _ => Ok(None),
        }
    });

    let project_routes = Router::new()
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
        );

    Router::new()
        .nest("/api/v1", project_routes)
        .layer(PolicyLayer::new(RequireAnyRole(&["admin", "member"])))
        .layer(AuthLayer::required().scheme(auth_scheme))
        .layer(TenantLayer::required(tenant_resolver))
        .layer(ObservabilityLayer::new())
}
