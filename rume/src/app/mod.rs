//! Application bootstrap, lifecycle, and developer-experience helpers.

pub mod builder;
pub mod context;
pub mod observability;
pub mod paths;
pub mod shutdown;
pub mod state;

// Re-export the public app surface at the module root for convenience.
pub use builder::{BuildError, IntoBindAddr, MiddlewareConfigError, Rume, RumeBuilder};
pub use context::{AppContext, AppContextBuilder};
pub use observability::{
    ObservabilityLayer, RequestId, X_REQUEST_ID, init_tracing, try_init_tracing,
};
pub use paths::ProjectPaths;
#[allow(deprecated)]
pub use shutdown::ops_router;
pub use state::{
    ApplicationMode, Availability, AvailabilityState, Environment, Lifecycle, LifecycleState,
};
