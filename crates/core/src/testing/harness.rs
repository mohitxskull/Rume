//! Declarative test harness coordinating database pools, lifecycle hooks, outbox relays, and router integration.

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use axum::body::Body;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use axum::http::Response;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use super::app::TestApp;

/// Declarative, fluent test harness coordinating database pools, lifecycle hooks, outbox relays, and test app routing.
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub struct RumeTestHarness<B: crate::persistence::DatabaseBackend + crate::outbox::OutboxRepository> {
    /// Active database pool configured for testing.
    pub pool: crate::persistence::DatabasePool<B>,
    /// Optional outbox relay configured for event dispatch.
    pub outbox: Option<std::sync::Arc<crate::outbox::OutboxRelay<crate::persistence::DatabasePool<B>>>>,
    /// Optional test app for issuing simulated HTTP requests.
    pub app: Option<TestApp>,
}

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
impl<B> RumeTestHarness<B>
where
    B: crate::persistence::DatabaseBackend + crate::outbox::OutboxRepository,
{
    /// Creates a test harness around an existing database pool.
    pub fn new(pool: crate::persistence::DatabasePool<B>) -> Self {
        Self {
            pool,
            outbox: None,
            app: None,
        }
    }

    /// Attaches an Axum router to the test harness.
    pub fn with_router(mut self, router: axum::Router<()>) -> Self {
        self.app = Some(TestApp::new(router));
        self
    }

    /// Attaches an outbox relay to the test harness.
    pub fn with_outbox<F, Fut>(mut self, dispatcher: F) -> Self
    where
        B: crate::outbox::OutboxRepository,
        F: Fn(&crate::outbox::OutboxMessage) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), crate::outbox::OutboxError>> + Send + 'static,
    {
        let relay = crate::outbox::OutboxRelay::new(std::sync::Arc::new(self.pool.clone()), dispatcher);
        self.outbox = Some(std::sync::Arc::new(relay));
        self
    }

    /// Returns a reference to the active test app, panicking if no router was configured.
    pub fn app(&self) -> &TestApp {
        self.app.as_ref().expect("no router configured on test harness")
    }

    /// Returns a reference to the active outbox relay, panicking if no outbox was configured.
    pub fn outbox(&self) -> &std::sync::Arc<crate::outbox::OutboxRelay<crate::persistence::DatabasePool<B>>> {
        self.outbox.as_ref().expect("no outbox configured on test harness")
    }

    /// Executes a SQL statement directly against the test harness database pool.
    pub async fn execute(&self, stmt: &str) -> Result<u64, crate::persistence::PersistenceError> {
        self.pool.backend().execute(stmt).await
    }

    /// Evaluates a scalar query directly against the test harness database pool.
    pub async fn query_scalar(&self, query: &str) -> Result<Option<String>, crate::persistence::PersistenceError> {
        self.pool.backend().query_scalar(query).await
    }

    /// Issue a GET request via the attached test app.
    pub async fn get(&self, uri: &str) -> Response<Body> {
        self.app().get(uri).await
    }

    /// Issue a POST JSON request via the attached test app.
    pub async fn post_json(&self, uri: &str, body: impl Into<String>) -> Response<Body> {
        self.app().post_json(uri, body).await
    }
}

#[cfg(feature = "sqlite")]
impl RumeTestHarness<crate::persistence::SqliteBackend> {
    /// Builds an in-memory SQLite test harness with an initialized pool.
    pub async fn in_memory() -> Result<Self, crate::persistence::PersistenceError> {
        let backend = crate::persistence::SqliteBackend::in_memory().await?;
        let config = crate::persistence::DatabaseConfig::new("memory://test");
        let pool = crate::persistence::DatabasePool::new(backend, config);
        Ok(Self::new(pool))
    }
}

#[cfg(feature = "postgres")]
impl RumeTestHarness<crate::persistence::PostgresBackend> {
    /// Builds a PostgreSQL test harness wrapping the provided pool.
    pub fn postgres(pool: crate::persistence::DatabasePool<crate::persistence::PostgresBackend>) -> Self {
        Self::new(pool)
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::routing::get;

    #[tokio::test]
    async fn test_rume_test_harness_in_memory_lifecycle() {
        let router = axum::Router::new().route("/ping", get(|| async { "pong" }));
        let harness = RumeTestHarness::in_memory()
            .await
            .expect("in-memory harness init")
            .with_router(router)
            .with_outbox(|_msg| async { Ok(()) });

        let resp = harness.get("/ping").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(TestApp::body_text(resp).await, "pong");

        // Execute query on harness pool
        let res = harness.execute("CREATE TABLE test_harness (id INT);").await;
        assert!(res.is_ok());

        assert!(harness.outbox.is_some());
    }
}
