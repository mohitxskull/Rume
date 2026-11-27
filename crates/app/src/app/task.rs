//! Application task registry and execution types for [`ApplicationMode::Task`](crate::app::state::ApplicationMode::Task).

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use crate::app::context::AppContext;

/// Metadata describing a registered application task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskInfo {
    /// Unique task identifier (e.g. `"users:sync"` or `"cleanup"`).
    pub name: String,
    /// Human-readable description shown by `rume task list`.
    pub description: String,
}

/// Boxed async task handler receiving [`AppContext`] and returning a fallible result.
pub type TaskHandler<E, C, S> = Box<
    dyn FnOnce(
            AppContext<E, C, S>,
        ) -> Pin<
            Box<dyn Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send>,
        > + Send,
>;

/// Errors returned by [`TaskRegistry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskError {
    /// A task with the same name was already registered.
    DuplicateTask(String),
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateTask(name) => write!(f, "duplicate task registration: '{name}'"),
        }
    }
}

impl std::error::Error for TaskError {}

struct TaskEntry<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    info: TaskInfo,
    handler: TaskHandler<E, C, S>,
}

/// Ordered registry of named application tasks.
pub struct TaskRegistry<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    entries: Vec<TaskEntry<E, C, S>>,
}

impl<E, C, S> Default for TaskRegistry<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<E, C, S> TaskRegistry<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    /// Create an empty task registry.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Register a named task, preserving insertion order and rejecting duplicates.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        handler: TaskHandler<E, C, S>,
    ) -> Result<(), TaskError> {
        let name = name.into();
        if self.entries.iter().any(|entry| entry.info.name == name) {
            return Err(TaskError::DuplicateTask(name));
        }
        self.entries.push(TaskEntry {
            info: TaskInfo {
                name,
                description: description.into(),
            },
            handler,
        });
        Ok(())
    }

    /// Return registered task metadata in insertion order.
    pub fn list(&self) -> Vec<TaskInfo> {
        self.entries
            .iter()
            .map(|entry| entry.info.clone())
            .collect()
    }

    /// Remove and return the handler for `name`, if registered.
    pub fn take(&mut self, name: &str) -> Option<TaskHandler<E, C, S>> {
        let idx = self
            .entries
            .iter()
            .position(|entry| entry.info.name == name)?;
        Some(self.entries.remove(idx).handler)
    }

    /// Returns `true` if no tasks are registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of registered tasks.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_registry_lists_registered_tasks_in_order() {
        let mut registry = TaskRegistry::<(), (), ()>::new();
        registry
            .register(
                "db:seed",
                "Seed initial database rows",
                Box::new(|_ctx| Box::pin(async { Ok(()) })),
            )
            .expect("first task should register");
        registry
            .register(
                "cache:warm",
                "Preload hot cache entries",
                Box::new(|_ctx| Box::pin(async { Ok(()) })),
            )
            .expect("second task should register");

        let listed = registry.list();
        assert_eq!(
            listed,
            vec![
                TaskInfo {
                    name: "db:seed".to_string(),
                    description: "Seed initial database rows".to_string(),
                },
                TaskInfo {
                    name: "cache:warm".to_string(),
                    description: "Preload hot cache entries".to_string(),
                },
            ]
        );
    }
}
