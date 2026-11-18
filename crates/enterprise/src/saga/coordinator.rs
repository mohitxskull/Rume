//! Saga workflow coordinator, runner, and persistence store.

use std::collections::HashMap;
use std::fmt::Debug;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::FutureExt;
use tokio::sync::RwLock;

use crate::saga::error::SagaError;
use crate::saga::step::{FunctionalStep, Step};

/// Pinned boxed future for saga store operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Status of a saga workflow execution.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SagaStatus {
    /// Initial pending state before execution begins.
    Pending,
    /// Currently executing a forward step.
    Running { current_step: String },
    /// All steps succeeded without errors.
    Success,
    /// Currently executing a compensation action.
    Compensating {
        current_step: String,
        /// Original forward error that triggered compensation, if known.
        // Why: Persisting the trigger error inside `Compensating` (with `#[serde(default)]`
        // for backward-compatible record deserialization) preserves the real forward failure
        // across crashes so `resume_compensation` does not overwrite it with a generic string.
        #[serde(default)]
        error: Option<SagaError>,
    },
    /// A step failed and all previously executed steps were successfully compensated.
    Compensated { error: SagaError },
    /// A step failed and at least one compensation action also failed.
    CompensationFailed {
        error: SagaError,
        compensation_error: SagaError,
    },
}

impl SagaStatus {
    /// Returns true if the saga completed all steps successfully.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }

    /// Returns true if the saga has reached a terminal outcome.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Success | Self::Compensated { .. } | Self::CompensationFailed { .. }
        )
    }
}

/// Persisted state record of a saga instance.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SagaRecord {
    /// Unique identifier for this saga instance execution.
    pub saga_id: String,
    /// Name of the saga workflow.
    pub saga_name: String,
    /// Current execution or terminal status.
    pub status: SagaStatus,
    /// List of step names that successfully executed forward.
    pub completed_steps: Vec<String>,
    /// List of step names that were compensated during rollback.
    pub compensated_steps: Vec<String>,
    /// Epoch timestamp (in seconds) of the last status update.
    pub updated_at: u64,
}

/// Storage contract for persisting saga execution records and state transitions.
pub trait SagaStore: Send + Sync + 'static {
    /// Saves or updates a saga execution record.
    fn save_record(&self, record: &SagaRecord) -> BoxFuture<'_, Result<(), SagaError>>;

    /// Loads a saga execution record by its unique ID.
    fn load_record(&self, saga_id: &str) -> BoxFuture<'_, Result<Option<SagaRecord>, SagaError>>;

    /// Lists all incomplete (non-terminal) saga executions for recovery.
    fn list_incomplete(&self) -> BoxFuture<'_, Result<Vec<SagaRecord>, SagaError>>;
}

/// Thread-safe in-memory saga storage suitable for testing and single-instance deployments.
#[derive(Debug, Clone, Default)]
pub struct InMemorySagaStore {
    records: Arc<RwLock<HashMap<String, SagaRecord>>>,
}

impl InMemorySagaStore {
    /// Creates a new in-memory saga store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl SagaStore for InMemorySagaStore {
    fn save_record(&self, record: &SagaRecord) -> BoxFuture<'_, Result<(), SagaError>> {
        let record = record.clone();
        let records = Arc::clone(&self.records);
        Box::pin(async move {
            let mut map = records.write().await;
            map.insert(record.saga_id.clone(), record);
            Ok(())
        })
    }

    fn load_record(&self, saga_id: &str) -> BoxFuture<'_, Result<Option<SagaRecord>, SagaError>> {
        let id = saga_id.to_string();
        let records = Arc::clone(&self.records);
        Box::pin(async move {
            let map = records.read().await;
            Ok(map.get(&id).cloned())
        })
    }

    fn list_incomplete(&self) -> BoxFuture<'_, Result<Vec<SagaRecord>, SagaError>> {
        let records = Arc::clone(&self.records);
        Box::pin(async move {
            let map = records.read().await;
            let list = map
                .values()
                .filter(|r| !r.status.is_terminal())
                .cloned()
                .collect();
            Ok(list)
        })
    }
}

/// Comprehensive report containing the updated context and execution trace.
pub struct SagaReport<Ctx> {
    /// The final mutated context.
    pub context: Ctx,
    /// Final status outcome.
    pub status: SagaStatus,
    /// List of step names that succeeded during forward execution.
    pub completed_steps: Vec<String>,
    /// List of step names that were compensated during rollback.
    pub compensated_steps: Vec<String>,
}

/// A composable multi-step saga workflow with automatic reverse compensation and persistence.
pub struct Saga<Ctx: Send + Sync + 'static> {
    name: String,
    steps: Vec<Arc<dyn Step<Ctx>>>,
    store: Option<Arc<dyn SagaStore>>,
}

impl<Ctx: Send + Sync + 'static> Saga<Ctx> {
    /// Creates a new saga workflow with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            steps: Vec::new(),
            store: None,
        }
    }

    /// Attaches a [`SagaStore`] persistence provider to this saga.
    pub fn with_store(mut self, store: Arc<dyn SagaStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Attaches an [`InMemorySagaStore`] to this saga.
    pub fn with_in_memory_store(self) -> Self {
        self.with_store(Arc::new(InMemorySagaStore::new()))
    }

    /// Returns the attached [`SagaStore`], if any.
    pub fn store(&self) -> Option<&Arc<dyn SagaStore>> {
        self.store.as_ref()
    }

    /// Adds a step implementing [`Step`].
    ///
    /// # Panics
    ///
    /// Panics if a step with the same name is already registered on this saga.
    pub fn add_step<S: Step<Ctx> + 'static>(mut self, step: S) -> Self {
        // Why: Reject duplicate step names at registration (panicking like
        // `HealthChecks::register` to preserve builder chaining) rather than allowing
        // ambiguous names or returning `Result` on every `.step()` call. `SagaRecord`
        // persists `completed_steps` and `compensated_steps` by name, so `resume_forward`
        // and `resume_compensation` resolve indices via `.find(|s| s.name() == name)` —
        // duplicate names would map to the wrong step index on recovery.
        let name = step.name();
        assert!(
            !self.steps.iter().any(|existing| existing.name() == name),
            "duplicate saga step name '{name}' in saga '{}'",
            self.name
        );
        self.steps.push(Arc::new(step));
        self
    }

    /// Adds a step from async closures.
    ///
    /// # Panics
    ///
    /// Panics if a step with the same name is already registered on this saga.
    pub fn step<A, C, FutA, FutC>(self, name: impl Into<String>, action: A, compensation: C) -> Self
    where
        A: Fn(&mut Ctx) -> FutA + Send + Sync + 'static,
        C: Fn(&mut Ctx) -> FutC + Send + Sync + 'static,
        FutA: std::future::Future<Output = Result<(), SagaError>> + Send + 'static,
        FutC: std::future::Future<Output = Result<(), SagaError>> + Send + 'static,
    {
        self.add_step(FunctionalStep::new(name, action, compensation))
    }

    /// Returns the name of this saga.
    pub fn name(&self) -> &str {
        &self.name
    }

    async fn persist_state(
        &self,
        saga_id: &str,
        status: SagaStatus,
        completed_steps: &[String],
        compensated_steps: &[String],
    ) -> Result<(), SagaError> {
        if let Some(store) = &self.store {
            let record = SagaRecord {
                saga_id: saga_id.to_string(),
                saga_name: self.name.clone(),
                status,
                completed_steps: completed_steps.to_vec(),
                compensated_steps: compensated_steps.to_vec(),
                updated_at: current_timestamp(),
            };
            // Why: Propagate `save_record` errors instead of logging and continuing, so a
            // persistence failure aborts the transition rather than leaving the store stale
            // while execution advances.
            store.save_record(&record).await?;
        }
        Ok(())
    }

    /// Executes the saga on the provided context with a randomly generated saga ID.
    ///
    /// # Errors
    ///
    /// Returns a [`SagaError`] if persisting state transitions to the attached [`SagaStore`] fails.
    pub async fn run(&self, ctx: Ctx) -> Result<SagaReport<Ctx>, SagaError> {
        let saga_id = uuid::Uuid::new_v4().to_string();
        self.run_with_id(&saga_id, ctx).await
    }

    /// Executes the saga on the provided context using an explicit saga ID for persistence tracking.
    ///
    /// # Errors
    ///
    /// Returns a [`SagaError`] if persisting state transitions to the attached [`SagaStore`] fails.
    pub async fn run_with_id(&self, saga_id: &str, ctx: Ctx) -> Result<SagaReport<Ctx>, SagaError> {
        self.persist_state(saga_id, SagaStatus::Pending, &[], &[])
            .await?;

        self.execute_forward_from(saga_id, ctx, 0, Vec::new(), Vec::new(), Vec::new())
            .await
    }

    /// Resumes an incomplete or crashed saga from its persisted state record.
    ///
    /// - If the saga was running, it resumes execution from the first uncompleted step.
    /// - If the saga was compensating, it resumes compensation for the remaining steps.
    /// - If the saga had already reached a terminal state, it returns the final report without re-executing.
    pub async fn resume(&self, saga_id: &str, ctx: Ctx) -> Result<SagaReport<Ctx>, SagaError> {
        let store = self.store.as_ref().ok_or_else(|| SagaError::Storage {
            message: "cannot resume saga without a configured SagaStore".into(),
        })?;

        let record = store
            .load_record(saga_id)
            .await?
            .ok_or_else(|| SagaError::Storage {
                message: format!("saga record '{saga_id}' not found"),
            })?;

        match record.status {
            SagaStatus::Success
            | SagaStatus::Compensated { .. }
            | SagaStatus::CompensationFailed { .. } => Ok(SagaReport {
                context: ctx,
                status: record.status,
                completed_steps: record.completed_steps,
                compensated_steps: record.compensated_steps,
            }),
            SagaStatus::Compensating { .. } => self.resume_compensation(saga_id, record, ctx).await,
            SagaStatus::Pending | SagaStatus::Running { .. } => {
                self.resume_forward(saga_id, record, ctx).await
            }
        }
    }

    fn resolve_completed_indices(&self, completed_names: &[String]) -> Vec<usize> {
        let mut indices = Vec::with_capacity(completed_names.len());
        for name in completed_names {
            if let Some((idx, _)) = self
                .steps
                .iter()
                .enumerate()
                .find(|(_, s)| s.name() == name)
            {
                indices.push(idx);
            }
        }
        indices
    }

    async fn execute_forward_from(
        &self,
        saga_id: &str,
        mut ctx: Ctx,
        start_idx: usize,
        mut completed_indices: Vec<usize>,
        mut completed_names: Vec<String>,
        compensated_names: Vec<String>,
    ) -> Result<SagaReport<Ctx>, SagaError> {
        let parent_tc = rume_observability::TraceContext::current();
        for (idx, step) in self.steps.iter().enumerate().skip(start_idx) {
            self.persist_state(
                saga_id,
                SagaStatus::Running {
                    current_step: step.name().to_string(),
                },
                &completed_names,
                &compensated_names,
            )
            .await?;

            let step_span = tracing::info_span!(
                "saga.step",
                saga.name = %self.name,
                saga.id = %saga_id,
                saga.step = %step.name(),
            );
            let fut = AssertUnwindSafe(step.execute(&mut ctx)).catch_unwind();
            let res = if let Some(ref tc) = parent_tc {
                tc.scope_future(step_span, fut).await
            } else {
                tracing::Instrument::instrument(fut, step_span).await
            };
            let step_result = match res {
                Ok(r) => r,
                Err(payload) => {
                    let msg = panic_message(&payload, "step execution panicked");
                    Err(SagaError::Aborted {
                        saga: self.name.clone(),
                        reason: format!("panic in step '{}': {msg}", step.name()),
                    })
                }
            };

            match step_result {
                Ok(()) => {
                    completed_indices.push(idx);
                    completed_names.push(step.name().to_string());
                    // Why: Persist completed step immediately after it succeeds rather than
                    // waiting for the next step's `Running` write, so a crash or persist
                    // failure between steps does not lose completion and re-run the step.
                    self.persist_state(
                        saga_id,
                        SagaStatus::Running {
                            current_step: step.name().to_string(),
                        },
                        &completed_names,
                        &compensated_names,
                    )
                    .await?;
                }
                Err(err) => {
                    return self
                        .compensate_completed(
                            saga_id,
                            ctx,
                            &completed_indices,
                            &completed_names,
                            compensated_names,
                            Some(err),
                        )
                        .await;
                }
            }
        }

        self.persist_state(
            saga_id,
            SagaStatus::Success,
            &completed_names,
            &compensated_names,
        )
        .await?;

        Ok(SagaReport {
            context: ctx,
            status: SagaStatus::Success,
            completed_steps: completed_names,
            compensated_steps: compensated_names,
        })
    }

    async fn compensate_completed(
        &self,
        saga_id: &str,
        mut ctx: Ctx,
        completed_indices: &[usize],
        completed_names: &[String],
        mut compensated_names: Vec<String>,
        original_error: Option<SagaError>,
    ) -> Result<SagaReport<Ctx>, SagaError> {
        let parent_tc = rume_observability::TraceContext::current();
        let mut first_comp_error = None;
        for &prev_idx in completed_indices.iter().rev() {
            let prev_step = &self.steps[prev_idx];
            if compensated_names.iter().any(|n| n == prev_step.name()) {
                continue;
            }

            self.persist_state(
                saga_id,
                SagaStatus::Compensating {
                    current_step: prev_step.name().to_string(),
                    error: original_error.clone(),
                },
                completed_names,
                &compensated_names,
            )
            .await?;

            let comp_span = tracing::info_span!(
                "saga.compensate",
                saga.name = %self.name,
                saga.id = %saga_id,
                saga.step = %prev_step.name(),
            );
            let fut = AssertUnwindSafe(prev_step.compensate(&mut ctx)).catch_unwind();
            let comp_res = if let Some(ref tc) = parent_tc {
                tc.scope_future(comp_span, fut).await
            } else {
                tracing::Instrument::instrument(fut, comp_span).await
            };
            let comp_outcome = match comp_res {
                Ok(r) => r,
                Err(payload) => {
                    let msg = panic_message(&payload, "compensation panicked");
                    Err(SagaError::Aborted {
                        saga: self.name.clone(),
                        reason: format!(
                            "panic in compensation for step '{}': {msg}",
                            prev_step.name()
                        ),
                    })
                }
            };

            match comp_outcome {
                Ok(()) => {
                    compensated_names.push(prev_step.name().to_string());
                    // Why: Persist immediately after each compensation succeeds so a crash
                    // before the next step's `Compensating` or terminal write does not
                    // re-run an already-completed compensation on resume.
                    self.persist_state(
                        saga_id,
                        SagaStatus::Compensating {
                            current_step: prev_step.name().to_string(),
                            error: original_error.clone(),
                        },
                        completed_names,
                        &compensated_names,
                    )
                    .await?;
                }
                Err(comp_err) => {
                    if first_comp_error.is_none() {
                        first_comp_error = Some(comp_err);
                    }
                }
            }
        }

        let status = match first_comp_error {
            Some(compensation_error) => SagaStatus::CompensationFailed {
                error: original_error.unwrap_or_else(|| SagaError::Aborted {
                    saga: self.name.clone(),
                    reason: "resumed compensation encountered failure".into(),
                }),
                compensation_error,
            },
            None => SagaStatus::Compensated {
                error: original_error.unwrap_or_else(|| SagaError::Aborted {
                    saga: self.name.clone(),
                    reason: "resumed compensation completed successfully".into(),
                }),
            },
        };

        self.persist_state(saga_id, status.clone(), completed_names, &compensated_names)
            .await?;

        Ok(SagaReport {
            context: ctx,
            status,
            completed_steps: completed_names.to_vec(),
            compensated_steps: compensated_names,
        })
    }

    async fn resume_forward(
        &self,
        saga_id: &str,
        record: SagaRecord,
        ctx: Ctx,
    ) -> Result<SagaReport<Ctx>, SagaError> {
        let completed_indices = self.resolve_completed_indices(&record.completed_steps);
        let start_idx = completed_indices.last().map(|i| i + 1).unwrap_or(0);

        self.execute_forward_from(
            saga_id,
            ctx,
            start_idx,
            completed_indices,
            record.completed_steps,
            record.compensated_steps,
        )
        .await
    }

    async fn resume_compensation(
        &self,
        saga_id: &str,
        record: SagaRecord,
        ctx: Ctx,
    ) -> Result<SagaReport<Ctx>, SagaError> {
        let completed_indices = self.resolve_completed_indices(&record.completed_steps);
        let original_error = match record.status {
            SagaStatus::Compensating { error, .. } => error,
            _ => None,
        };

        self.compensate_completed(
            saga_id,
            ctx,
            &completed_indices,
            &record.completed_steps,
            record.compensated_steps,
            original_error,
        )
        .await
    }

    /// Forces rollback and compensation for all completed steps recorded under `saga_id`.
    pub async fn compensate_crashed(
        &self,
        saga_id: &str,
        ctx: Ctx,
    ) -> Result<SagaReport<Ctx>, SagaError> {
        let store = self.store.as_ref().ok_or_else(|| SagaError::Storage {
            message: "cannot compensate crashed saga without a configured SagaStore".into(),
        })?;

        let record = store
            .load_record(saga_id)
            .await?
            .ok_or_else(|| SagaError::Storage {
                message: format!("saga record '{saga_id}' not found"),
            })?;

        self.resume_compensation(saga_id, record, ctx).await
    }
}

fn panic_message<'a>(payload: &'a Box<dyn std::any::Any + Send>, fallback: &'a str) -> &'a str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
        .unwrap_or(fallback)
}
