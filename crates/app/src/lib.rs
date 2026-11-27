//! Application builder, lifecycle management, and modular composition for Rume.

#![forbid(unsafe_code)]

pub mod app;
pub mod module;

pub use app::{
    AppContext, AppContextBuilder, ApplicationMode, Availability, AvailabilityState, BuildError,
    Environment, IntoBindAddr, Lifecycle, LifecycleState, MiddlewareConfigError, ProjectPaths,
    RequestId, Rume, RumeBuilder, TaskError, TaskHandler, TaskInfo, TaskRegistry, X_REQUEST_ID,
    init_tracing, shutdown_global_observability, try_init_tracing,
};
pub use module::{
    ArchitectureRules, ArchitectureViolation, BoxFuture, Module, ModuleContext, ModuleError,
    ModuleHookResult, ModuleRegistry,
};
