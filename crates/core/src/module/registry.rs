//! Module registry and dependency graph resolution.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::Module;
use super::error::ModuleError;

/// Registry that verifies module architecture, detects circular dependencies,
/// and determines topological startup and shutdown sequences.
#[derive(Default, Clone)]
pub struct ModuleRegistry {
    modules: Vec<Arc<dyn Module>>,
}

impl ModuleRegistry {
    /// Create an empty module registry.
    pub fn new() -> Self {
        Self {
            modules: Vec::new(),
        }
    }

    /// Register a module in the application.
    pub fn register(&mut self, module: Arc<dyn Module>) -> Result<(), ModuleError> {
        let name = module.name();
        if self.modules.iter().any(|m| m.name() == name) {
            return Err(ModuleError::DuplicateModule {
                name: name.to_string(),
            });
        }
        self.modules.push(module);
        Ok(())
    }

    /// Access all registered modules.
    pub fn modules(&self) -> &[Arc<dyn Module>] {
        &self.modules
    }

    /// Resolve the topological startup order for all registered modules.
    ///
    /// Dependencies appear **before** the modules that depend on them.
    ///
    /// # Errors
    /// Returns [`ModuleError::MissingDependency`] if a declared dependency is missing.
    /// Returns [`ModuleError::CircularDependency`] if a dependency cycle is detected.
    pub fn resolve_startup_order(&self) -> Result<Vec<&str>, ModuleError> {
        let mut deps_map: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for m in &self.modules {
            deps_map.insert(m.name(), m.dependencies());
        }

        // 1. Verify that all declared dependencies are registered.
        for (name, deps) in &deps_map {
            for dep in deps {
                if !deps_map.contains_key(dep) {
                    return Err(ModuleError::MissingDependency {
                        module: (*name).to_string(),
                        dependency: (*dep).to_string(),
                    });
                }
            }
        }

        // 2. Cycle detection & topological sort via post-order DFS.
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum State {
            Unvisited,
            Visiting,
            Visited,
        }

        let mut states: BTreeMap<&str, State> = BTreeMap::new();
        for name in deps_map.keys() {
            states.insert(name, State::Unvisited);
        }

        let mut order: Vec<&str> = Vec::with_capacity(self.modules.len());
        let mut path: Vec<&str> = Vec::new();

        fn dfs<'a>(
            node: &'a str,
            deps_map: &BTreeMap<&'a str, Vec<&'a str>>,
            states: &mut BTreeMap<&'a str, State>,
            path: &mut Vec<&'a str>,
            order: &mut Vec<&'a str>,
        ) -> Result<(), ModuleError> {
            states.insert(node, State::Visiting);
            path.push(node);

            let deps = &deps_map[node];
            for &dep in deps {
                match states.get(dep).copied().unwrap_or(State::Unvisited) {
                    State::Visiting => {
                        // Cycle detected! Extract the cycle path.
                        let cycle_start = path.iter().position(|&n| n == dep).unwrap_or(0);
                        let mut cycle: Vec<String> =
                            path[cycle_start..].iter().map(|&s| s.to_string()).collect();
                        cycle.push(dep.to_string());
                        return Err(ModuleError::CircularDependency { cycle });
                    }
                    State::Unvisited => {
                        dfs(dep, deps_map, states, path, order)?;
                    }
                    State::Visited => {}
                }
            }

            path.pop();
            states.insert(node, State::Visited);
            order.push(node);
            Ok(())
        }

        // Visit modules in insertion order
        for m in &self.modules {
            let name = m.name();
            if states.get(name) == Some(&State::Unvisited) {
                dfs(name, &deps_map, &mut states, &mut path, &mut order)?;
            }
        }

        Ok(order)
    }

    /// Resolve the shutdown order for all registered modules.
    ///
    /// This is the exact reverse of the startup order: dependents shut down
    /// **before** the dependencies they rely on.
    pub fn resolve_shutdown_order(&self) -> Result<Vec<&str>, ModuleError> {
        let mut startup = self.resolve_startup_order()?;
        startup.reverse();
        Ok(startup)
    }

    /// Resolve all registered modules in topological startup order.
    pub fn resolve_startup_modules(&self) -> Result<Vec<Arc<dyn Module>>, ModuleError> {
        let order = self.resolve_startup_order()?;
        let mut map: BTreeMap<&str, Arc<dyn Module>> = BTreeMap::new();
        for m in &self.modules {
            map.insert(m.name(), Arc::clone(m));
        }
        Ok(order.into_iter().map(|name| map[name].clone()).collect())
    }

    /// Resolve all registered modules in reverse topological shutdown order.
    pub fn resolve_shutdown_modules(&self) -> Result<Vec<Arc<dyn Module>>, ModuleError> {
        let mut modules = self.resolve_startup_modules()?;
        modules.reverse();
        Ok(modules)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyModule {
        name: &'static str,
        deps: Vec<&'static str>,
    }

    impl DummyModule {
        #[allow(clippy::new_ret_no_self)]
        fn new(name: &'static str, deps: &[&'static str]) -> Arc<dyn Module> {
            Arc::new(Self {
                name,
                deps: deps.to_vec(),
            })
        }
    }

    impl Module for DummyModule {
        fn name(&self) -> &str {
            self.name
        }

        fn dependencies(&self) -> Vec<&str> {
            self.deps.clone()
        }
    }

    #[test]
    fn detects_duplicate_modules() {
        let mut reg = ModuleRegistry::new();
        reg.register(DummyModule::new("users", &[])).unwrap();
        let err = reg.register(DummyModule::new("users", &[])).unwrap_err();
        assert_eq!(
            err,
            ModuleError::DuplicateModule {
                name: "users".to_string()
            }
        );
    }

    #[test]
    fn detects_missing_dependencies() {
        let mut reg = ModuleRegistry::new();
        reg.register(DummyModule::new("billing", &["users"]))
            .unwrap();
        let err = reg.resolve_startup_order().unwrap_err();
        assert_eq!(
            err,
            ModuleError::MissingDependency {
                module: "billing".to_string(),
                dependency: "users".to_string(),
            }
        );
    }

    #[test]
    fn detects_two_node_circular_dependency() {
        let mut reg = ModuleRegistry::new();
        reg.register(DummyModule::new("a", &["b"])).unwrap();
        reg.register(DummyModule::new("b", &["a"])).unwrap();
        let err = reg.resolve_startup_order().unwrap_err();
        match err {
            ModuleError::CircularDependency { cycle } => {
                assert_eq!(cycle, vec!["a", "b", "a"]);
            }
            other => panic!("expected CircularDependency, got {other:?}"),
        }
    }

    #[test]
    fn detects_three_node_circular_dependency() {
        let mut reg = ModuleRegistry::new();
        reg.register(DummyModule::new("orders", &["billing"]))
            .unwrap();
        reg.register(DummyModule::new("billing", &["users"]))
            .unwrap();
        reg.register(DummyModule::new("users", &["orders"]))
            .unwrap();
        let err = reg.resolve_startup_order().unwrap_err();
        match err {
            ModuleError::CircularDependency { cycle } => {
                assert_eq!(cycle, vec!["orders", "billing", "users", "orders"]);
            }
            other => panic!("expected CircularDependency, got {other:?}"),
        }
    }

    #[test]
    fn orders_dependencies_before_dependents() {
        let mut reg = ModuleRegistry::new();
        // Register in reverse order: billing first, then users
        reg.register(DummyModule::new("billing", &["users"]))
            .unwrap();
        reg.register(DummyModule::new("users", &[])).unwrap();

        let startup = reg.resolve_startup_order().unwrap();
        assert_eq!(startup, vec!["users", "billing"]);

        let shutdown = reg.resolve_shutdown_order().unwrap();
        assert_eq!(shutdown, vec!["billing", "users"]);
    }

    #[test]
    fn complex_dag_topological_sort() {
        let mut reg = ModuleRegistry::new();
        // Graph:
        // auth (no deps)
        // users (depends on auth)
        // billing (depends on users)
        // notifications (depends on users)
        // orders (depends on billing, notifications)
        reg.register(DummyModule::new("orders", &["billing", "notifications"]))
            .unwrap();
        reg.register(DummyModule::new("billing", &["users"]))
            .unwrap();
        reg.register(DummyModule::new("notifications", &["users"]))
            .unwrap();
        reg.register(DummyModule::new("users", &["auth"])).unwrap();
        reg.register(DummyModule::new("auth", &[])).unwrap();

        let startup = reg.resolve_startup_order().unwrap();

        // Verify order invariants:
        let pos = |name: &str| startup.iter().position(|&n| n == name).unwrap();
        assert!(pos("auth") < pos("users"));
        assert!(pos("users") < pos("billing"));
        assert!(pos("users") < pos("notifications"));
        assert!(pos("billing") < pos("orders"));
        assert!(pos("notifications") < pos("orders"));
    }
}
