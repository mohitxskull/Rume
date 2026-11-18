//! CI module dependency graph verification and architectural boundary assertions.
//!
//! Provides verification of declared module dependency graphs (inspired by ArchUnit
//! and Spring Modulith).
//!
//! ### Boundary Enforcement Model
//! - **Declared graph validation**: [`ArchitectureRules`] checks declared relationships
//!   from [`Module::dependencies()`], verifying transitive closure reachability, forbidden
//!   domain couplings, and required core modules.
//! - **Source-level call enforcement**: Checking declared dependencies does not prevent
//!   unregistered direct source calls. True physical encapsulation in Rust must be enforced
//!   using module visibility (`pub(crate)`, private sub-modules) or separate Cargo crate boundaries.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
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

        // 2. Verify all declared dependencies exist in the verified module set (fail-closed against unprovided modules)
        for module in modules {
            for dep in module.dependencies() {
                if !registered_names.contains(&dep) {
                    return Err(ArchitectureViolation {
                        rule: "undeclared_dependency".to_string(),
                        details: format!(
                            "module '{}' depends on '{dep}', which is missing from the verified module set",
                            module.name()
                        ),
                    });
                }
            }
        }

        // 3. Build dependency graph and detect cycles
        let mut dep_graph: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for module in modules {
            dep_graph.insert(module.name(), module.dependencies());
        }

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum DfsState {
            Unvisited,
            Visiting,
            Visited,
        }

        let mut states: BTreeMap<&str, DfsState> = BTreeMap::new();
        for module in modules {
            states.insert(module.name(), DfsState::Unvisited);
        }

        fn check_cycle<'a>(
            node: &'a str,
            dep_graph: &BTreeMap<&'a str, Vec<&'a str>>,
            states: &mut BTreeMap<&'a str, DfsState>,
        ) -> Result<(), ArchitectureViolation> {
            states.insert(node, DfsState::Visiting);
            if let Some(deps) = dep_graph.get(node) {
                for dep in deps {
                    match states.get(dep) {
                        Some(DfsState::Visiting) => {
                            return Err(ArchitectureViolation {
                                rule: "circular_dependency".to_string(),
                                details: format!(
                                    "circular dependency detected involving module '{node}' and '{dep}'"
                                ),
                            });
                        }
                        Some(DfsState::Unvisited) => {
                            check_cycle(dep, dep_graph, states)?;
                        }
                        _ => {}
                    }
                }
            }
            states.insert(node, DfsState::Visited);
            Ok(())
        }

        for module in modules {
            if states.get(module.name()) == Some(&DfsState::Unvisited) {
                check_cycle(module.name(), &dep_graph, &mut states)?;
            }
        }

        // 4. Compute transitive closure
        let mut transitive_deps: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for module in modules {
            let mut visited = BTreeSet::new();
            let mut queue = VecDeque::new();
            for dep in module.dependencies() {
                queue.push_back(dep);
            }

            while let Some(current) = queue.pop_front() {
                if visited.insert(current)
                    && let Some(next_deps) = dep_graph.get(current)
                {
                    for next in next_deps {
                        queue.push_back(next);
                    }
                }
            }
            transitive_deps.insert(module.name(), visited);
        }

        // 5. Verify forbidden dependencies against direct and transitive dependencies
        for (forbidden_from, forbidden_to) in &self.forbidden_deps {
            if let Some(all_deps) = transitive_deps.get(forbidden_from.as_str())
                && all_deps.contains(forbidden_to.as_str())
            {
                return Err(ArchitectureViolation {
                    rule: "forbid_dependency".to_string(),
                    details: format!(
                        "module '{forbidden_from}' is forbidden from depending on '{forbidden_to}', but reaches it as a dependency"
                    ),
                });
            }
        }

        Ok(())
    }
}
