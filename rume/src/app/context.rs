//! Application context holding env, config, paths, availability, lifecycle,
//! health checks, and application state.
//!
//! Every Rume application is built with an [`AppContext`]. Handlers access env,
//! config, paths, availability, health checks, and application state through
//! [`State<AppContext<E, C, S>>`].

use crate::app::paths::ProjectPaths;
use crate::app::state::Availability;
use crate::routing::{HealthChecks, RouteRegistry};

/// Application context holding env, config, paths, availability, lifecycle,
/// health checks, and application state.
///
/// # Example
///
/// ```no_run
/// use rume::app::{AppContext, Availability, ProjectPaths, Rume};
/// use rume::routing::{HealthChecks, Router};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let ctx = AppContext::new((), (), ProjectPaths::from_current_dir(), Availability::new(), HealthChecks::new(), ());
/// let router = Router::new();
///
/// Rume::builder(ctx).router(router).run().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct AppContext<E, C, S = ()>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    /// Loaded environment variables.
    pub env: E,
    /// Loaded application configuration.
    pub config: C,
    /// Project directory layout.
    pub paths: ProjectPaths,
    /// Application availability state (live / ready / refusing / broken).
    pub availability: Availability,
    /// Application lifecycle state.
    pub lifecycle: crate::app::state::Lifecycle,
    /// Health check registry.
    pub health_checks: HealthChecks,
    /// Application-scoped route registry (for `rume list:routes`).
    pub route_registry: RouteRegistry,
    /// Application state (db pool, redis, HTTP client, …). Defaults to `()`.
    pub state: S,
}

impl<E, C, S> AppContext<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    /// Create a new application context with all required fields.
    pub fn new(
        env: E,
        config: C,
        paths: ProjectPaths,
        availability: Availability,
        health_checks: HealthChecks,
        state: S,
    ) -> Self {
        Self {
            env,
            config,
            paths,
            availability,
            lifecycle: crate::app::state::Lifecycle::new(),
            health_checks,
            route_registry: RouteRegistry::new(),
            state,
        }
    }
}

impl<E, C> AppContext<E, C, ()>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
{
    /// Start building an `AppContext` with default `state = ()`.
    ///
    /// Required: `env` and `config`. Optional: `paths`, `availability`, `health_checks`, `state`.
    /// Defaults: `ProjectPaths::from_current_dir()`, `Availability::new()`, `HealthChecks::new()`, `state = ()`.
    pub fn builder(env: E, config: C) -> AppContextBuilder<E, C, ()> {
        AppContextBuilder {
            env,
            config,
            paths: None,
            availability: None,
            health_checks: None,
            route_registry: None,
            state: (),
        }
    }

    /// Convenience constructor with sensible default paths, availability, health checks, and `state = ()`.
    pub fn new_with_defaults(env: E, config: C) -> Self {
        Self::builder(env, config).build()
    }
}

/// Builder for [`AppContext`] with optional defaults.
pub struct AppContextBuilder<E, C, S = ()>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    env: E,
    config: C,
    paths: Option<ProjectPaths>,
    availability: Option<crate::app::state::Availability>,
    health_checks: Option<HealthChecks>,
    route_registry: Option<RouteRegistry>,
    state: S,
}

impl<E, C, S> AppContextBuilder<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    /// Set the project paths (default: `ProjectPaths::from_current_dir()`).
    pub fn paths(mut self, paths: ProjectPaths) -> Self {
        self.paths = Some(paths);
        self
    }

    /// Set the availability handle (default: `Availability::new()`).
    pub fn availability(mut self, availability: Availability) -> Self {
        self.availability = Some(availability);
        self
    }

    /// Set the health checks registry (default: `HealthChecks::new()`).
    pub fn health_checks(mut self, health_checks: HealthChecks) -> Self {
        self.health_checks = Some(health_checks);
        self
    }

    /// Set the route registry (default: `RouteRegistry::new()`).
    pub fn route_registry(mut self, route_registry: RouteRegistry) -> Self {
        self.route_registry = Some(route_registry);
        self
    }

    /// Transition to custom application state (db pool, redis, HTTP client, …).
    ///
    /// Does NOT require `NewS: Default`.
    pub fn state<NewS>(self, state: NewS) -> AppContextBuilder<E, C, NewS>
    where
        NewS: Clone + Send + Sync + 'static,
    {
        AppContextBuilder {
            env: self.env,
            config: self.config,
            paths: self.paths,
            availability: self.availability,
            health_checks: self.health_checks,
            route_registry: self.route_registry,
            state,
        }
    }

    /// Build the `AppContext`.
    pub fn build(self) -> AppContext<E, C, S> {
        AppContext {
            env: self.env,
            config: self.config,
            paths: self.paths.unwrap_or_else(ProjectPaths::from_current_dir),
            availability: self.availability.unwrap_or_default(),
            lifecycle: crate::app::state::Lifecycle::new(),
            health_checks: self.health_checks.unwrap_or_default(),
            route_registry: self.route_registry.unwrap_or_default(),
            state: self.state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::LifecycleState;

    #[test]
    fn app_context_builder_with_defaults() {
        // D-M1 fix: AppContext::builder() provides ergonomic construction with defaults.
        let ctx: AppContext<_, _, ()> = AppContext::builder(
            std::net::IpAddr::from([127, 0, 0, 1]),
            std::collections::BTreeMap::<String, String>::new(),
        )
        .build();

        assert_eq!(ctx.env, std::net::IpAddr::from([127, 0, 0, 1]));
        assert!(ctx.config.is_empty());
        assert!(!ctx.availability.is_ready()); // Unknown state
        assert_eq!(ctx.lifecycle.state(), LifecycleState::Created);
    }

    #[test]
    fn app_context_builder_custom_paths() {
        // D-M1 fix: builder allows overriding defaults.
        let custom_paths = ProjectPaths::from_root("/custom/root");

        let ctx: AppContext<_, _, ()> = AppContext::builder(
            std::net::IpAddr::from([127, 0, 0, 1]),
            std::collections::BTreeMap::<String, String>::new(),
        )
        .paths(custom_paths.clone())
        .build();

        assert_eq!(ctx.paths.root, custom_paths.root);
        assert_eq!(ctx.paths.config, custom_paths.config);
        assert_eq!(ctx.paths.public, custom_paths.public);
    }

    #[derive(Clone, PartialEq, Eq, Debug)]
    struct CustomNonDefaultState {
        id: u64,
    }

    #[test]
    fn app_context_builder_non_default_state_and_inference() {
        // No explicit type annotation required on builder call
        let ctx = AppContext::builder(
            "production".to_string(),
            std::collections::BTreeMap::<String, String>::new(),
        )
        .state(CustomNonDefaultState { id: 42 })
        .build();

        assert_eq!(ctx.state, CustomNonDefaultState { id: 42 });
        assert_eq!(ctx.env, "production");
    }
}
