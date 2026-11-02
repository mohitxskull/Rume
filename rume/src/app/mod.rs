//! Application bootstrap, lifecycle, and developer-experience helpers.

pub mod builder;
pub mod context;
pub mod observability;
pub mod paths;
pub mod shutdown;
pub mod state;

// Re-export the public app surface at the module root for convenience.
pub use builder::{MiddlewareConfigError, Rume, RumeBuilder};
pub use context::{AppContext, AppContextBuilder};
pub use observability::{ObservabilityLayer, RequestId, X_REQUEST_ID};
pub use paths::ProjectPaths;
pub use shutdown::{init_tracing, ops_router};
pub use state::{
    ApplicationMode, Availability, AvailabilityState, Environment, Lifecycle, LifecycleState,
};
