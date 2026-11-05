//! Scoped context provided to modules during lifecycle hooks.

use crate::app::paths::ProjectPaths;
use crate::app::state::{Availability, Lifecycle};

/// Scoped context provided to a [`super::Module`] during initialization and shutdown.
#[derive(Clone, Debug)]
pub struct ModuleContext {
    pub module_name: String,
    pub paths: ProjectPaths,
    pub availability: Availability,
    pub lifecycle: Lifecycle,
}

impl ModuleContext {
    /// Create a new module context.
    pub fn new(
        module_name: impl Into<String>,
        paths: ProjectPaths,
        availability: Availability,
        lifecycle: Lifecycle,
    ) -> Self {
        Self {
            module_name: module_name.into(),
            paths,
            availability,
            lifecycle,
        }
    }

    /// Access the module's registered name.
    pub fn module_name(&self) -> &str {
        &self.module_name
    }

    /// Access project file paths.
    pub fn paths(&self) -> &ProjectPaths {
        &self.paths
    }

    /// Access the atomic application availability state.
    pub fn availability(&self) -> &Availability {
        &self.availability
    }

    /// Access the atomic application lifecycle state.
    pub fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }
}
