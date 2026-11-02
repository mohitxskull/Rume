//! CI architecture verification rules and boundary enforcement.
//!
//! Provides static verification of module boundaries and dependencies (inspired by ArchUnit and
//! Spring Modulith), ensuring that forbidden cross-domain calls and architecture violations fail in CI.

use std::fmt;

use crate::module::Module;

/// An architectural violation detected during verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchitectureViolation {
    pub rule: String,
    pub details: String,
}

impl fmt::Display for ArchitectureViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "architecture violation [{}]: {}",
            self.rule, self.details
        )
    }
}

impl std::error::Error for ArchitectureViolation {}

/// Rules builder for asserting module boundary and dependency integrity.
#[derive(Debug, Default, Clone)]
pub struct ArchitectureRules {
    forbidden_deps: Vec<(String, String)>,
    required_modules: Vec<String>,
}

impl ArchitectureRules {
    /// Creates a new rules set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares that `from_module` is forbidden from depending on `to_module`.
    pub fn forbid_dependency(
        mut self,
        from_module: impl Into<String>,
        to_module: impl Into<String>,
    ) -> Self {
        self.forbidden_deps
            .push((from_module.into(), to_module.into()));
        self
    }

    /// Asserts that a module with `name` must be registered in the application.
    pub fn require_module(mut self, name: impl Into<String>) -> Self {
        self.required_modules.push(name.into());
        self
    }

    /// Verifies the rules against a collection of modules.
    pub fn verify(&self, modules: &[&dyn Module]) -> Result<(), ArchitectureViolation> {
        let registered_names: Vec<&str> = modules.iter().map(|m| m.name()).collect();

        // 1. Verify required modules
        for required in &self.required_modules {
            if !registered_names.contains(&required.as_str()) {
                return Err(ArchitectureViolation {
                    rule: "require_module".to_string(),
                    details: format!(
                        "required module '{required}' is missing from the application"
                    ),
                });
            }
        }

        // 2. Verify forbidden dependencies
        for module in modules {
            let from_name = module.name();
            let declared_deps = module.dependencies();

            for (forbidden_from, forbidden_to) in &self.forbidden_deps {
                if from_name == forbidden_from && declared_deps.contains(&forbidden_to.as_str()) {
                    return Err(ArchitectureViolation {
                        rule: "forbid_dependency".to_string(),
                        details: format!(
                            "module '{from_name}' is forbidden from depending on '{forbidden_to}', but declares it as a dependency"
                        ),
                    });
                }
            }
        }

        Ok(())
    }
}
