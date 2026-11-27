//! Axum router integration for `async-graphql`.

use async_graphql::http::GraphiQLSource;
use async_graphql::{ObjectType, Schema, SchemaBuilder, SubscriptionType};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::Router;
use axum::extract::State;
use axum::http::Extensions;
use axum::response::Html;
use axum::routing::{get, post};
use rume_foundation::RequestId;
use rume_observability::TraceContext;
use rume_security::auth::AuthUser;
use rume_security::tenant::Tenant;

/// Configuration for [`graphql_routes_from_builder`] and [`GraphQlConfig::apply_limits`].
///
/// Enforces `max_depth`, `max_complexity`, and `introspection` limits on a [`SchemaBuilder`].
#[derive(Debug, Clone)]
pub struct GraphQlConfig {
    pub path: String,
    pub graphiql_enabled: bool,
    pub introspection: bool,
    pub max_depth: Option<usize>,
    pub max_complexity: Option<usize>,
}

impl Default for GraphQlConfig {
    fn default() -> Self {
        Self {
            path: "/graphql".to_string(),
            graphiql_enabled: false,
            introspection: false,
            max_depth: Some(16),
            max_complexity: Some(256),
        }
    }
}

impl GraphQlConfig {
    /// Creates a new [`GraphQlConfig`] with default limits at the given endpoint path.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }

    /// Enables or disables the GraphiQL GET UI.
    pub fn with_graphiql(mut self, enabled: bool) -> Self {
        self.graphiql_enabled = enabled;
        self
    }

    /// Enables or disables GraphQL schema introspection (default: `false` for production hardening).
    pub fn with_introspection(mut self, enabled: bool) -> Self {
        self.introspection = enabled;
        self
    }

    /// Configures maximum query depth.
    pub fn with_max_depth(mut self, max_depth: Option<usize>) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Configures maximum query complexity.
    pub fn with_max_complexity(mut self, max_complexity: Option<usize>) -> Self {
        self.max_complexity = max_complexity;
        self
    }

    /// Applies configured `max_depth`, `max_complexity`, and `introspection` limits to an
    /// `async-graphql` [`SchemaBuilder`].
    pub fn apply_limits<Q, M, S>(
        &self,
        mut builder: SchemaBuilder<Q, M, S>,
    ) -> SchemaBuilder<Q, M, S> {
        if !self.introspection {
            builder = builder.disable_introspection();
        }
        if let Some(max_depth) = self.max_depth {
            builder = builder.limit_depth(max_depth);
        }
        if let Some(max_complexity) = self.max_complexity {
            builder = builder.limit_complexity(max_complexity);
        }
        builder
    }
}

async fn graphql_handler<Q, M, S>(
    State(schema): State<Schema<Q, M, S>>,
    extensions: Extensions,
    req: GraphQLRequest,
) -> GraphQLResponse
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
{
    // Why: forward Axum middleware context (`AuthUser`, `Tenant`, `RequestId`, `TraceContext`)
    // into `async_graphql::Request::data(...)` so resolvers can enforce auth/tenant boundaries
    // and correlate traces without re-parsing HTTP headers (`RC-24`).
    let mut gql_req = req.into_inner();
    if let Some(request_id) = extensions.get::<RequestId>().cloned() {
        gql_req = gql_req.data(request_id);
    }
    if let Some(trace_ctx) = extensions
        .get::<TraceContext>()
        .cloned()
        .or_else(TraceContext::current)
    {
        gql_req = gql_req.data(trace_ctx);
    }
    if let Some(user) = extensions.get::<AuthUser>().cloned() {
        gql_req = gql_req.data(user);
    }
    if let Some(tenant) = extensions.get::<Tenant>().cloned() {
        gql_req = gql_req.data(tenant);
    }
    schema.execute(gql_req).await.into()
}

fn build_graphql_router<Q, M, S>(schema: Schema<Q, M, S>, config: GraphQlConfig) -> Router
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
{
    let path = config.path.clone();
    let mut router = Router::new()
        .route(&path, post(graphql_handler::<Q, M, S>))
        .with_state(schema);

    if config.graphiql_enabled {
        let endpoint = config.path;
        router = router.route(
            &path,
            get(move || async move { Html(GraphiQLSource::build().endpoint(&endpoint).finish()) }),
        );
    }

    router
}

/// Builds an Axum [`Router`] from a [`SchemaBuilder`], applying
/// [`GraphQlConfig::max_depth`], [`GraphQlConfig::max_complexity`], and
/// [`GraphQlConfig::introspection`] via [`GraphQlConfig::apply_limits`] before
/// finishing the schema.
pub fn graphql_routes_from_builder<Q, M, S>(
    builder: SchemaBuilder<Q, M, S>,
    config: GraphQlConfig,
) -> Router
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
{
    let schema = config.apply_limits(builder).finish();
    build_graphql_router(schema, config)
}
