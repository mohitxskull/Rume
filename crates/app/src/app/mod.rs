//! Application bootstrap, lifecycle, and developer-experience helpers.

pub mod builder;
pub mod context;
pub mod observability;
pub use rume_foundation::paths;
pub mod shutdown;
pub use rume_foundation::state;

// Re-export the public app surface at the module root for convenience.
pub use builder::{BuildError, IntoBindAddr, MiddlewareConfigError, Rume, RumeBuilder};
pub use context::{AppContext, AppContextBuilder};
pub use observability::{
    ObservabilityLayer, RequestId, TRACEPARENT, TRACESTATE, TraceContext, X_REQUEST_ID,
    init_tracing, try_init_tracing,
};
pub use paths::ProjectPaths;
pub use state::{
    ApplicationMode, Availability, AvailabilityState, Environment, Lifecycle, LifecycleState,
};
