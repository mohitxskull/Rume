//! Axum router integration for `async-graphql`.

use async_graphql::http::GraphiQLSource;
use async_graphql::{ObjectType, Schema, SchemaBuilder, SubscriptionType};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::Router;
use axum::extract::State;
use axum::response::Html;
use axum::routing::{get, post};

/// Configuration for [`graphql_routes`] and [`graphql_routes_from_builder`].
///
/// Note: [`graphql_routes`] accepts a pre-built [`Schema`] and cannot
/// retroactively apply `max_depth` or `max_complexity`. Use
/// [`graphql_routes_from_builder`] or [`GraphQlConfig::apply_limits`] to
/// enforce these limits on a [`SchemaBuilder`].
#[derive(Debug, Clone)]
pub struct GraphQlConfig {
    pub path: String,
    pub graphiql_enabled: bool,
    pub max_depth: Option<usize>,
    pub max_complexity: Option<usize>,
}

impl Default for GraphQlConfig {
    fn default() -> Self {
        Self {
            path: "/graphql".to_string(),
            graphiql_enabled: false,
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

    /// Applies configured `max_depth` and `max_complexity` limits to an
    /// `async-graphql` [`SchemaBuilder`].
    pub fn apply_limits<Q, M, S>(
        &self,
        mut builder: SchemaBuilder<Q, M, S>,
    ) -> SchemaBuilder<Q, M, S> {
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
    req: GraphQLRequest,
) -> GraphQLResponse
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
{
    schema.execute(req.into_inner()).await.into()
}

/// Builds an Axum [`Router`] serving GraphQL POST queries/mutations and optional GraphiQL GET UI
/// from a pre-built [`Schema`].
///
/// Pre-built schemas do not automatically inherit [`GraphQlConfig::max_depth`] or
/// [`GraphQlConfig::max_complexity`]; use [`graphql_routes_from_builder`] or
/// [`GraphQlConfig::apply_limits`] when building the schema to enforce DoS limits.
pub fn graphql_routes<Q, M, S>(schema: Schema<Q, M, S>, config: GraphQlConfig) -> Router
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
/// [`GraphQlConfig::max_depth`] and [`GraphQlConfig::max_complexity`] via
/// [`GraphQlConfig::apply_limits`] before finishing the schema.
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
    graphql_routes(schema, config)
}
