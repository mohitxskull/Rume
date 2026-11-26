//! Axum router integration for `async-graphql`.

use async_graphql::http::GraphiQLSource;
use async_graphql::{ObjectType, Schema, SubscriptionType};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::Router;
use axum::extract::State;
use axum::response::Html;
use axum::routing::{get, post};

/// Configuration for [`graphql_routes`].
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

/// Builds an Axum [`Router`] serving GraphQL POST queries/mutations and optional GraphiQL GET UI.
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
