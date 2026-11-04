use axum::routing::{get, post};
use rume::app::observability::ObservabilityLayer;
use rume::auth::policy::RequireAnyRole;
use rume::auth::{AuthLayer, BearerTokenScheme, PolicyLayer};
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;
use rume::resilience::{RateLimitConfig, RateLimitLayer, RateLimiter};
use rume::routing::Router;
use rume::tenant::resolver::HeaderTenantResolver;
use rume::tenant::{TenantLayer, TenantMembershipLayer};

use crate::handlers::{health, issues, organizations, projects, users};
use crate::repositories::UserRepository;
use crate::state::WorkbenchState;

pub fn build_api_router(
    pool: DatabasePool<PostgresBackend>,
    tenant_resolver: HeaderTenantResolver,
) -> Router<WorkbenchState> {
    let auth_pool = pool.clone();
    let auth_scheme = BearerTokenScheme::new(move |token: String| {
        let pool = auth_pool.clone();
        async move {
            UserRepository::find_by_token(&pool, &token)
                .await
                .map_err(|e| rume::auth::error::AuthError::ProviderError(e.to_string()))
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

    let org_limiter = RateLimiter::new(RateLimitConfig::default());

    let public_org_routes = Router::new()
        .route(
            "/organizations",
            post(organizations::create_organization).get(organizations::list_organizations),
        )
        .route(
            "/organizations/{id}",
            get(organizations::get_organization),
        )
        .layer(RateLimitLayer::new(org_limiter));

    Router::new()
        .nest("/api/v1", tenant_scoped_routes)
        .nest("/api/v1", public_org_routes)
        .route("/health", get(health::health))
        .layer(ObservabilityLayer::new())
}
