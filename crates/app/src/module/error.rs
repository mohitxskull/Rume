//! Errors related to module registration, lifecycle, and architecture verification.

use std::fmt;

/// Errors that can occur when verifying or executing application modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleError {
    /// A module with the same name was registered more than once.
    DuplicateModule { name: String },

    /// A module depends on another module that was not registered in the application.
    MissingDependency { module: String, dependency: String },

    /// A circular dependency was detected between modules.
    CircularDependency { cycle: Vec<String> },

    /// A module failed during its startup hook.
    StartupFailed { module: String, message: String },
}

impl fmt::Display for ModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateModule { name } => {
                write!(f, "module '{name}' is registered more than once")
            }
            Self::MissingDependency { module, dependency } => {
                write!(
                    f,
                    "module '{module}' depends on module '{dependency}', but '{dependency}' is not registered"
                )
            }
            Self::CircularDependency { cycle } => {
                write!(
                    f,
                    "circular module dependency detected: {}",
                    cycle.join(" -> ")
                )
            }
            Self::StartupFailed { module, message } => {
                write!(f, "module '{module}' failed to start: {message}")
            }
        }
    }
}

impl std::error::Error for ModuleError {}

impl From<ModuleError> for std::io::Error {
    fn from(err: ModuleError) -> Self {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, err.to_string())
    }
}
