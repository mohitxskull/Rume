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

/// Builds a deterministic forward-execution idempotency key for a `(saga_id, step_name)` pair.
pub fn forward_idempotency_key(saga_id: &str, step_name: &str) -> String {
    format!("saga:{saga_id}:forward:{step_name}")
}

/// Builds a deterministic compensation idempotency key for a `(saga_id, step_name)` pair.
pub fn compensation_idempotency_key(saga_id: &str, step_name: &str) -> String {
    format!("saga:{saga_id}:compensate:{step_name}")
}

fn attach_saga_id(err: SagaError, saga_id: &str) -> SagaError {
    match err {
        SagaError::Storage {
            saga_id: None,
            message,
        } => SagaError::Storage {
            saga_id: Some(saga_id.to_string()),
            message,
        },
        other => other,
    }
}

/// Execution phase captured in a [`SagaRecoveryHandle`] when persistence fails.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SagaRecoveryPhase {
    /// Forward execution was in progress.
    Forward,
    /// Rollback/compensation was in progress.
    Compensating {
        /// Original forward error that triggered compensation, if known.
        error: Option<SagaError>,
        /// First compensation error encountered so far, if any.
        compensation_error: Option<SagaError>,
    },
}

/// In-memory recovery handle returned when a saga persistence transition fails.
///
/// Retains the `saga_id`, the mutated `context`, the execution `phase` (including
/// rollback intent and triggering error), and the completed/compensated step lists so
/// an in-process caller can retry via [`Saga::resume_from_handle`] without losing
/// in-memory context mutations or reverting from rollback back to forward execution.
///
/// # Crash recovery
///
/// If the process crashes before [`Saga::resume_from_handle`] persists state, in-memory
/// `Ctx` changes cannot be retained. Callers invoking [`Saga::resume`] after a process
/// crash must reconstruct `Ctx` from domain state, use [`Saga::step_with_key`] (or
/// [`Step::execute_with_key`] / [`Step::compensate_with_key`]) for steps with external
/// side effects, and call [`Saga::compensate_crashed`] if rollback intent had not yet
/// been durably persisted to the [`SagaStore`].
pub struct SagaRecoveryHandle<Ctx> {
    /// Identifier of the saga instance.
    pub saga_id: String,
    /// Mutated context preserved at the point of failure.
    pub context: Ctx,
    /// Phase the saga was in when the error occurred.
    pub phase: SagaRecoveryPhase,
    /// Step names that completed forward execution.
    pub completed_steps: Vec<String>,
    /// Step names that completed compensation.
    pub compensated_steps: Vec<String>,
}

impl<Ctx> Debug for SagaRecoveryHandle<Ctx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SagaRecoveryHandle")
            .field("saga_id", &self.saga_id)
            .field("phase", &self.phase)
            .field("completed_steps", &self.completed_steps)
            .field("compensated_steps", &self.compensated_steps)
            .finish_non_exhaustive()
    }
}

/// Error returned by recoverable saga execution methods, pairing the [`SagaError`]
/// with a [`SagaRecoveryHandle`] containing the mutated context and recovery state.
pub struct SagaRecoveryError<Ctx> {
    /// Underlying saga error.
    pub error: SagaError,
    /// Recovery handle retaining `saga_id`, mutated `context`, and execution phase.
    pub handle: SagaRecoveryHandle<Ctx>,
}

impl<Ctx> Debug for SagaRecoveryError<Ctx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SagaRecoveryError")
            .field("error", &self.error)
            .field("handle", &self.handle)
            .finish()
    }
}

impl<Ctx> std::fmt::Display for SagaRecoveryError<Ctx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (saga_id: {})", self.error, self.handle.saga_id)
    }
}

impl<Ctx> std::error::Error for SagaRecoveryError<Ctx> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
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

    /// Adds a step from async closures that receive a stable per-saga/step idempotency key.
    ///
    /// # Panics
    ///
    /// Panics if a step with the same name is already registered on this saga.
    pub fn step_with_key<A, C, FutA, FutC>(
        self,
        name: impl Into<String>,
        action: A,
        compensation: C,
    ) -> Self
    where
        A: Fn(&mut Ctx, String) -> FutA + Send + Sync + 'static,
        C: Fn(&mut Ctx, String) -> FutC + Send + Sync + 'static,
        FutA: std::future::Future<Output = Result<(), SagaError>> + Send + 'static,
        FutC: std::future::Future<Output = Result<(), SagaError>> + Send + 'static,
    {
        self.add_step(FunctionalStep::with_key(name, action, compensation))
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
            // Why: Propagate `save_record` errors (annotated with `saga_id`) instead of
            // logging and continuing, so a persistence failure aborts the transition rather
            // than leaving the store stale while execution advances.
            store
                .save_record(&record)
                .await
                .map_err(|err| attach_saga_id(err, saga_id))?;
        }
        Ok(())
    }

    /// Executes the saga on the provided context with a randomly generated saga ID.
    ///
    /// # Errors
    ///
    /// Returns a [`SagaError`] (with `saga_id` populated on [`SagaError::Storage`]) if
    /// persisting state transitions to the attached [`SagaStore`] fails.
    ///
    /// Callers that need the mutated `Ctx` or rollback intent preserved across transient
    /// storage failures should use [`Saga::run_recoverable`].
    pub async fn run(&self, ctx: Ctx) -> Result<SagaReport<Ctx>, SagaError> {
        let saga_id = uuid::Uuid::new_v4().to_string();
        self.run_with_id(&saga_id, ctx).await
    }

    /// Executes the saga with a randomly generated saga ID, returning a [`SagaRecoveryError`]
    /// that preserves `saga_id`, the mutated `Ctx`, and rollback phase on failure.
    pub async fn run_recoverable(
        &self,
        ctx: Ctx,
    ) -> Result<SagaReport<Ctx>, SagaRecoveryError<Ctx>> {
        let saga_id = uuid::Uuid::new_v4().to_string();
        self.run_with_id_recoverable(&saga_id, ctx).await
    }

    /// Executes the saga on the provided context using an explicit saga ID for persistence tracking.
    ///
    /// # Errors
    ///
    /// Returns a [`SagaError`] (with `saga_id` populated on [`SagaError::Storage`]) if
    /// persisting state transitions to the attached [`SagaStore`] fails.
    pub async fn run_with_id(&self, saga_id: &str, ctx: Ctx) -> Result<SagaReport<Ctx>, SagaError> {
        self.run_with_id_recoverable(saga_id, ctx)
            .await
            .map_err(|err| err.error)
    }

    /// Executes the saga using an explicit saga ID, returning a [`SagaRecoveryError`]
    /// that preserves the mutated `Ctx` and rollback phase on storage failure.
    pub async fn run_with_id_recoverable(
        &self,
        saga_id: &str,
        ctx: Ctx,
    ) -> Result<SagaReport<Ctx>, SagaRecoveryError<Ctx>> {
        if let Err(error) = self
            .persist_state(saga_id, SagaStatus::Pending, &[], &[])
            .await
        {
            return Err(SagaRecoveryError {
                error,
                handle: SagaRecoveryHandle {
                    saga_id: saga_id.to_string(),
                    context: ctx,
                    phase: SagaRecoveryPhase::Forward,
                    completed_steps: Vec::new(),
                    compensated_steps: Vec::new(),
                },
            });
        }

        self.execute_forward_from_recoverable(saga_id, ctx, 0, Vec::new(), Vec::new(), Vec::new())
            .await
    }

    /// Resumes an incomplete or crashed saga from its persisted state record.
    ///
    /// - If the saga was running, it resumes execution from the first uncompleted step.
    /// - If the saga was compensating, it resumes compensation for the remaining steps.
    /// - If the saga had already reached a terminal state, it returns the final report without re-executing.
    ///
    /// # Crash recovery
    ///
    /// After a process crash, in-memory `Ctx` mutations from earlier steps cannot be
    /// recovered from `SagaRecord` alone; callers must reconstruct `Ctx` from domain state
    /// and use [`Saga::step_with_key`] (or [`Step::execute_with_key`] /
    /// [`Step::compensate_with_key`]) for steps with external side effects. If the crash
    /// occurred before the first `Compensating` record was persisted, call
    /// [`Saga::compensate_crashed`] instead of `resume`.
    pub async fn resume(&self, saga_id: &str, ctx: Ctx) -> Result<SagaReport<Ctx>, SagaError> {
        self.resume_recoverable(saga_id, ctx)
            .await
            .map_err(|err| err.error)
    }

    /// Resumes an incomplete or crashed saga from its persisted state record, returning a
    /// [`SagaRecoveryError`] that preserves `Ctx` and phase if a storage write fails.
    pub async fn resume_recoverable(
        &self,
        saga_id: &str,
        ctx: Ctx,
    ) -> Result<SagaReport<Ctx>, SagaRecoveryError<Ctx>> {
        let store = match self.store.as_ref() {
            Some(store) => store,
            None => {
                return Err(SagaRecoveryError {
                    error: SagaError::Storage {
                        saga_id: Some(saga_id.to_string()),
                        message: "cannot resume saga without a configured SagaStore".into(),
                    },
                    handle: SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Forward,
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                    },
                });
            }
        };

        let record = match store.load_record(saga_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return Err(SagaRecoveryError {
                    error: SagaError::Storage {
                        saga_id: Some(saga_id.to_string()),
                        message: format!("saga record '{saga_id}' not found"),
                    },
                    handle: SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Forward,
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                    },
                });
            }
            Err(err) => {
                return Err(SagaRecoveryError {
                    error: attach_saga_id(err, saga_id),
                    handle: SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Forward,
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                    },
                });
            }
        };

        match record.status {
            SagaStatus::Success
            | SagaStatus::Compensated { .. }
            | SagaStatus::CompensationFailed { .. } => Ok(SagaReport {
                context: ctx,
                status: record.status,
                completed_steps: record.completed_steps,
                compensated_steps: record.compensated_steps,
            }),
            SagaStatus::Compensating { .. } => {
                self.resume_compensation_recoverable(saga_id, record, ctx)
                    .await
            }
            SagaStatus::Pending | SagaStatus::Running { .. } => {
                self.resume_forward_recoverable(saga_id, record, ctx).await
            }
        }
    }

    /// Resumes an in-process saga from a [`SagaRecoveryHandle`] returned by a previous
    /// [`SagaRecoveryError`], preserving the mutated `Ctx`, completed/compensated steps,
    /// and rollback intent.
    pub async fn resume_from_handle(
        &self,
        handle: SagaRecoveryHandle<Ctx>,
    ) -> Result<SagaReport<Ctx>, SagaRecoveryError<Ctx>> {
        let completed_indices = self.resolve_completed_indices(&handle.completed_steps);
        match handle.phase {
            SagaRecoveryPhase::Forward => {
                let start_idx = completed_indices.last().map(|i| i + 1).unwrap_or(0);
                self.execute_forward_from_recoverable(
                    &handle.saga_id,
                    handle.context,
                    start_idx,
                    completed_indices,
                    handle.completed_steps,
                    handle.compensated_steps,
                )
                .await
            }
            SagaRecoveryPhase::Compensating {
                error,
                compensation_error,
            } => {
                self.compensate_completed_recoverable(
                    &handle.saga_id,
                    handle.context,
                    &completed_indices,
                    &handle.completed_steps,
                    handle.compensated_steps,
                    error,
                    compensation_error,
                )
                .await
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

    async fn execute_forward_from_recoverable(
        &self,
        saga_id: &str,
        mut ctx: Ctx,
        start_idx: usize,
        mut completed_indices: Vec<usize>,
        mut completed_names: Vec<String>,
        compensated_names: Vec<String>,
    ) -> Result<SagaReport<Ctx>, SagaRecoveryError<Ctx>> {
        let parent_tc = rume_observability::TraceContext::current();
        for (idx, step) in self.steps.iter().enumerate().skip(start_idx) {
            if let Err(error) = self
                .persist_state(
                    saga_id,
                    SagaStatus::Running {
                        current_step: step.name().to_string(),
                    },
                    &completed_names,
                    &compensated_names,
                )
                .await
            {
                return Err(SagaRecoveryError {
                    error,
                    handle: SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Forward,
                        completed_steps: completed_names,
                        compensated_steps: compensated_names,
                    },
                });
            }

            let step_span = tracing::info_span!(
                "saga.step",
                saga.name = %self.name,
                saga.id = %saga_id,
                saga.step = %step.name(),
            );
            let idempotency_key = forward_idempotency_key(saga_id, step.name());
            let fut =
                AssertUnwindSafe(step.execute_with_key(&mut ctx, &idempotency_key)).catch_unwind();
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
                    if let Err(error) = self
                        .persist_state(
                            saga_id,
                            SagaStatus::Running {
                                current_step: step.name().to_string(),
                            },
                            &completed_names,
                            &compensated_names,
                        )
                        .await
                    {
                        return Err(SagaRecoveryError {
                            error,
                            handle: SagaRecoveryHandle {
                                saga_id: saga_id.to_string(),
                                context: ctx,
                                phase: SagaRecoveryPhase::Forward,
                                completed_steps: completed_names,
                                compensated_steps: compensated_names,
                            },
                        });
                    }
                }
                Err(err) => {
                    return self
                        .compensate_completed_recoverable(
                            saga_id,
                            ctx,
                            &completed_indices,
                            &completed_names,
                            compensated_names,
                            Some(err),
                            None,
                        )
                        .await;
                }
            }
        }

        if let Err(error) = self
            .persist_state(
                saga_id,
                SagaStatus::Success,
                &completed_names,
                &compensated_names,
            )
            .await
        {
            return Err(SagaRecoveryError {
                error,
                handle: SagaRecoveryHandle {
                    saga_id: saga_id.to_string(),
                    context: ctx,
                    phase: SagaRecoveryPhase::Forward,
                    completed_steps: completed_names,
                    compensated_steps: compensated_names,
                },
            });
        }

        Ok(SagaReport {
            context: ctx,
            status: SagaStatus::Success,
            completed_steps: completed_names,
            compensated_steps: compensated_names,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn compensate_completed_recoverable(
        &self,
        saga_id: &str,
        mut ctx: Ctx,
        completed_indices: &[usize],
        completed_names: &[String],
        mut compensated_names: Vec<String>,
        original_error: Option<SagaError>,
        initial_comp_error: Option<SagaError>,
    ) -> Result<SagaReport<Ctx>, SagaRecoveryError<Ctx>> {
        let parent_tc = rume_observability::TraceContext::current();
        let mut first_comp_error = initial_comp_error;
        for &prev_idx in completed_indices.iter().rev() {
            let prev_step = &self.steps[prev_idx];
            if compensated_names.iter().any(|n| n == prev_step.name()) {
                continue;
            }

            if let Err(error) = self
                .persist_state(
                    saga_id,
                    SagaStatus::Compensating {
                        current_step: prev_step.name().to_string(),
                        error: original_error.clone(),
                    },
                    completed_names,
                    &compensated_names,
                )
                .await
            {
                return Err(SagaRecoveryError {
                    error,
                    handle: SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Compensating {
                            error: original_error,
                            compensation_error: first_comp_error,
                        },
                        completed_steps: completed_names.to_vec(),
                        compensated_steps: compensated_names,
                    },
                });
            }

            let comp_span = tracing::info_span!(
                "saga.compensate",
                saga.name = %self.name,
                saga.id = %saga_id,
                saga.step = %prev_step.name(),
            );
            let comp_key = compensation_idempotency_key(saga_id, prev_step.name());
            let fut =
                AssertUnwindSafe(prev_step.compensate_with_key(&mut ctx, &comp_key)).catch_unwind();
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
                    if let Err(error) = self
                        .persist_state(
                            saga_id,
                            SagaStatus::Compensating {
                                current_step: prev_step.name().to_string(),
                                error: original_error.clone(),
                            },
                            completed_names,
                            &compensated_names,
                        )
                        .await
                    {
                        return Err(SagaRecoveryError {
                            error,
                            handle: SagaRecoveryHandle {
                                saga_id: saga_id.to_string(),
                                context: ctx,
                                phase: SagaRecoveryPhase::Compensating {
                                    error: original_error,
                                    compensation_error: first_comp_error,
                                },
                                completed_steps: completed_names.to_vec(),
                                compensated_steps: compensated_names,
                            },
                        });
                    }
                }
                Err(comp_err) => {
                    if first_comp_error.is_none() {
                        first_comp_error = Some(comp_err);
                    }
                }
            }
        }

        let resolved_error = original_error.unwrap_or_else(|| SagaError::Aborted {
            saga: self.name.clone(),
            reason: if first_comp_error.is_some() {
                "resumed compensation encountered failure".into()
            } else {
                "resumed compensation completed successfully".into()
            },
        });
        let status = match &first_comp_error {
            Some(compensation_error) => SagaStatus::CompensationFailed {
                error: resolved_error.clone(),
                compensation_error: compensation_error.clone(),
            },
            None => SagaStatus::Compensated {
                error: resolved_error.clone(),
            },
        };

        if let Err(error) = self
            .persist_state(saga_id, status.clone(), completed_names, &compensated_names)
            .await
        {
            return Err(SagaRecoveryError {
                error,
                handle: SagaRecoveryHandle {
                    saga_id: saga_id.to_string(),
                    context: ctx,
                    phase: SagaRecoveryPhase::Compensating {
                        error: Some(resolved_error),
                        compensation_error: first_comp_error,
                    },
                    completed_steps: completed_names.to_vec(),
                    compensated_steps: compensated_names,
                },
            });
        }

        Ok(SagaReport {
            context: ctx,
            status,
            completed_steps: completed_names.to_vec(),
            compensated_steps: compensated_names,
        })
    }

    async fn resume_forward_recoverable(
        &self,
        saga_id: &str,
        record: SagaRecord,
        ctx: Ctx,
    ) -> Result<SagaReport<Ctx>, SagaRecoveryError<Ctx>> {
        let completed_indices = self.resolve_completed_indices(&record.completed_steps);
        let start_idx = completed_indices.last().map(|i| i + 1).unwrap_or(0);

        self.execute_forward_from_recoverable(
            saga_id,
            ctx,
            start_idx,
            completed_indices,
            record.completed_steps,
            record.compensated_steps,
        )
        .await
    }

    async fn resume_compensation_recoverable(
        &self,
        saga_id: &str,
        record: SagaRecord,
        ctx: Ctx,
    ) -> Result<SagaReport<Ctx>, SagaRecoveryError<Ctx>> {
        let completed_indices = self.resolve_completed_indices(&record.completed_steps);
        let original_error = match record.status {
            SagaStatus::Compensating { error, .. } => error,
            _ => None,
        };

        self.compensate_completed_recoverable(
            saga_id,
            ctx,
            &completed_indices,
            &record.completed_steps,
            record.compensated_steps,
            original_error,
            None,
        )
        .await
    }

    /// Forces rollback and compensation for all completed steps recorded under `saga_id`.
    pub async fn compensate_crashed(
        &self,
        saga_id: &str,
        ctx: Ctx,
    ) -> Result<SagaReport<Ctx>, SagaError> {
        self.compensate_crashed_recoverable(saga_id, ctx)
            .await
            .map_err(|err| err.error)
    }

    /// Forces rollback and compensation for all completed steps recorded under `saga_id`,
    /// returning a [`SagaRecoveryError`] that preserves `Ctx` and rollback phase on storage failure.
    pub async fn compensate_crashed_recoverable(
        &self,
        saga_id: &str,
        ctx: Ctx,
    ) -> Result<SagaReport<Ctx>, SagaRecoveryError<Ctx>> {
        let store = match self.store.as_ref() {
            Some(store) => store,
            None => {
                return Err(SagaRecoveryError {
                    error: SagaError::Storage {
                        saga_id: Some(saga_id.to_string()),
                        message: "cannot compensate crashed saga without a configured SagaStore"
                            .into(),
                    },
                    handle: SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Compensating {
                            error: None,
                            compensation_error: None,
                        },
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                    },
                });
            }
        };

        let record = match store.load_record(saga_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return Err(SagaRecoveryError {
                    error: SagaError::Storage {
                        saga_id: Some(saga_id.to_string()),
                        message: format!("saga record '{saga_id}' not found"),
                    },
                    handle: SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Compensating {
                            error: None,
                            compensation_error: None,
                        },
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                    },
                });
            }
            Err(err) => {
                return Err(SagaRecoveryError {
                    error: attach_saga_id(err, saga_id),
                    handle: SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Compensating {
                            error: None,
                            compensation_error: None,
                        },
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                    },
                });
            }
        };

        self.resume_compensation_recoverable(saga_id, record, ctx)
            .await
    }
}

fn panic_message<'a>(payload: &'a Box<dyn std::any::Any + Send>, fallback: &'a str) -> &'a str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type FailPred = Arc<dyn Fn(&SagaRecord) -> bool + Send + Sync>;

    #[derive(Clone, Default)]
    struct FaultySagaStore {
        inner: InMemorySagaStore,
        fail_on: Arc<Mutex<Option<FailPred>>>,
    }

    impl FaultySagaStore {
        fn new() -> Self {
            Self::default()
        }

        fn fail_when<F>(&self, pred: F)
        where
            F: Fn(&SagaRecord) -> bool + Send + Sync + 'static,
        {
            *self.fail_on.lock().expect("lock") = Some(Arc::new(pred));
        }

        fn clear_failure(&self) {
            *self.fail_on.lock().expect("lock") = None;
        }
    }

    impl SagaStore for FaultySagaStore {
        fn save_record(&self, record: &SagaRecord) -> BoxFuture<'_, Result<(), SagaError>> {
            let should_fail = self
                .fail_on
                .lock()
                .expect("lock")
                .as_ref()
                .is_some_and(|pred| pred(record));
            if should_fail {
                return Box::pin(async {
                    Err(SagaError::Storage {
                        saga_id: None,
                        message: "injected storage failure".into(),
                    })
                });
            }
            self.inner.save_record(record)
        }

        fn load_record(
            &self,
            saga_id: &str,
        ) -> BoxFuture<'_, Result<Option<SagaRecord>, SagaError>> {
            self.inner.load_record(saga_id)
        }

        fn list_incomplete(&self) -> BoxFuture<'_, Result<Vec<SagaRecord>, SagaError>> {
            self.inner.list_incomplete()
        }
    }

    #[tokio::test]
    async fn run_includes_generated_saga_id_in_storage_error() {
        let store = Arc::new(FaultySagaStore::new());
        store.fail_when(|r| {
            matches!(&r.status, SagaStatus::Running { current_step } if current_step == "step_2")
        });

        let saga = Saga::new("id_in_storage_err")
            .with_store(store.clone())
            .step("step_1", |_| async { Ok(()) }, |_| async { Ok(()) })
            .step("step_2", |_| async { Ok(()) }, |_| async { Ok(()) });

        let err1 = saga
            .run(())
            .await
            .err()
            .expect("first run should fail on step_2 Running write");
        let err2 = saga
            .run(())
            .await
            .err()
            .expect("second run should fail on step_2 Running write");

        let id1 = err1
            .saga_id()
            .expect("storage error must include generated saga_id")
            .to_string();
        let id2 = err2
            .saga_id()
            .expect("storage error must include generated saga_id")
            .to_string();
        assert_ne!(id1, id2, "each run must report its own distinct saga_id");

        store.clear_failure();
        let report1 = saga.resume(&id1, ()).await.expect("resume 1");
        let report2 = saga.resume(&id2, ()).await.expect("resume 2");
        assert!(report1.status.is_success());
        assert!(report2.status.is_success());
    }

    #[tokio::test]
    async fn run_recoverable_preserves_mutated_context_across_storage_failure() {
        #[derive(Default)]
        struct Ctx {
            reservation_id: Option<String>,
            observed_by_b: Option<String>,
        }

        let store = Arc::new(FaultySagaStore::new());
        store.fail_when(|r| {
            matches!(&r.status, SagaStatus::Running { current_step } if current_step == "step_b")
        });

        let a_runs = Arc::new(AtomicUsize::new(0));
        let a_runs_c = Arc::clone(&a_runs);

        let saga = Saga::new("preserve_ctx")
            .with_store(store.clone())
            .step(
                "step_a",
                move |ctx: &mut Ctx| {
                    a_runs_c.fetch_add(1, Ordering::SeqCst);
                    ctx.reservation_id = Some("resv-42".into());
                    async { Ok(()) }
                },
                |_| async { Ok(()) },
            )
            .step(
                "step_b",
                |ctx: &mut Ctx| {
                    ctx.observed_by_b = ctx.reservation_id.clone();
                    async { Ok(()) }
                },
                |_| async { Ok(()) },
            );

        let err = saga
            .run_recoverable(Ctx::default())
            .await
            .err()
            .expect("step_b Running write should fail");
        assert_eq!(
            err.handle.context.reservation_id.as_deref(),
            Some("resv-42")
        );
        assert_eq!(err.handle.completed_steps, vec!["step_a"]);

        store.clear_failure();
        let report = saga
            .resume_from_handle(err.handle)
            .await
            .expect("resume_from_handle should succeed");
        assert!(report.status.is_success());
        assert_eq!(a_runs.load(Ordering::SeqCst), 1, "step_a must not re-run");
        assert_eq!(report.context.observed_by_b.as_deref(), Some("resv-42"));
    }

    #[tokio::test]
    async fn forward_recovery_deduplicates_when_post_step_completion_write_fails() {
        let store = Arc::new(FaultySagaStore::new());
        // Reject the post-step completion write for step_a itself.
        store.fail_when(|r| {
            matches!(&r.status, SagaStatus::Running { current_step } if current_step == "step_a")
                && r.completed_steps == vec!["step_a".to_string()]
        });

        let applied_keys = Arc::new(Mutex::new(HashSet::<String>::new()));
        let reservations_created = Arc::new(AtomicUsize::new(0));

        let applied_c = Arc::clone(&applied_keys);
        let created_c = Arc::clone(&reservations_created);

        let saga = Saga::new("post_step_fail_dedup")
            .with_store(store.clone())
            .step_with_key(
                "step_a",
                move |_ctx: &mut (), key: String| {
                    let applied = Arc::clone(&applied_c);
                    let created = Arc::clone(&created_c);
                    async move {
                        if applied.lock().expect("lock").insert(key) {
                            created.fetch_add(1, Ordering::SeqCst);
                        }
                        Ok(())
                    }
                },
                |_, _| async { Ok(()) },
            )
            .step("step_b", |_| async { Ok(()) }, |_| async { Ok(()) });

        let saga_id = "saga-post-step-fail-1";
        let err = saga
            .run_with_id(saga_id, ())
            .await
            .err()
            .expect("post-step completion write must fail");
        assert_eq!(err.saga_id(), Some(saga_id));
        assert_eq!(reservations_created.load(Ordering::SeqCst), 1);

        // Even with ordinary `resume` (simulating a crash where the handle was lost),
        // the stable idempotency key prevents duplicating the reservation.
        store.clear_failure();
        let report = saga.resume(saga_id, ()).await.expect("resume must succeed");
        assert!(report.status.is_success());
        assert_eq!(
            reservations_created.load(Ordering::SeqCst),
            1,
            "reservation must not be duplicated on resume"
        );
    }

    #[tokio::test]
    async fn rollback_intent_preserved_when_first_compensating_write_fails() {
        let store = Arc::new(FaultySagaStore::new());
        // Reject the very first Compensating write.
        store.fail_when(|r| matches!(&r.status, SagaStatus::Compensating { .. }));

        let step_b_attempts = Arc::new(AtomicUsize::new(0));
        let step_b_attempts_c = Arc::clone(&step_b_attempts);
        let step_a_compensated = Arc::new(AtomicUsize::new(0));
        let step_a_comp_c = Arc::clone(&step_a_compensated);

        let original_err = SagaError::ActionFailed {
            step: "step_b".into(),
            message: "payment declined".into(),
        };
        let original_err_c = original_err.clone();

        let saga = Saga::new("rollback_intent")
            .with_store(store.clone())
            .step(
                "step_a",
                |_| async { Ok(()) },
                move |_| {
                    step_a_comp_c.fetch_add(1, Ordering::SeqCst);
                    async { Ok(()) }
                },
            )
            .step(
                "step_b",
                move |_| {
                    let attempt = step_b_attempts_c.fetch_add(1, Ordering::SeqCst);
                    let err = original_err_c.clone();
                    async move {
                        if attempt == 0 {
                            Err(err)
                        } else {
                            // Would succeed if erroneously re-run!
                            Ok(())
                        }
                    }
                },
                |_| async { Ok(()) },
            );

        let err = saga
            .run_recoverable(())
            .await
            .err()
            .expect("first Compensating write must fail");
        assert!(matches!(
            err.handle.phase,
            SagaRecoveryPhase::Compensating {
                error: Some(ref e),
                compensation_error: None
            } if *e == original_err
        ));

        store.clear_failure();
        let report = saga
            .resume_from_handle(err.handle)
            .await
            .expect("resume_from_handle must complete rollback");
        assert_eq!(
            report.status,
            SagaStatus::Compensated {
                error: original_err
            }
        );
        assert_eq!(
            step_b_attempts.load(Ordering::SeqCst),
            1,
            "step_b must not be re-executed after it already failed"
        );
        assert_eq!(step_a_compensated.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn compensation_recovery_deduplicates_when_post_comp_write_fails() {
        let store = Arc::new(FaultySagaStore::new());
        // Reject the post-compensation completion write immediately after step_b's undo succeeds.
        store.fail_when(|r| {
            matches!(
                &r.status,
                SagaStatus::Compensating { current_step, .. } if current_step == "step_b"
            ) && r.compensated_steps == vec!["step_b".to_string()]
        });

        let comp_keys = Arc::new(Mutex::new(HashSet::<String>::new()));
        let b_releases = Arc::new(AtomicUsize::new(0));
        let a_releases = Arc::new(AtomicUsize::new(0));

        let comp_keys_c = Arc::clone(&comp_keys);
        let b_releases_c = Arc::clone(&b_releases);
        let a_releases_c = Arc::clone(&a_releases);

        let trigger_err = SagaError::ActionFailed {
            step: "step_c".into(),
            message: "boom".into(),
        };
        let trigger_err_c = trigger_err.clone();

        let saga = Saga::new("post_comp_fail_dedup")
            .with_store(store.clone())
            .step(
                "step_a",
                |_| async { Ok(()) },
                move |_| {
                    a_releases_c.fetch_add(1, Ordering::SeqCst);
                    async { Ok(()) }
                },
            )
            .step_with_key(
                "step_b",
                |_, _| async { Ok(()) },
                move |_, key: String| {
                    let keys = Arc::clone(&comp_keys_c);
                    let releases = Arc::clone(&b_releases_c);
                    async move {
                        if keys.lock().expect("lock").insert(key) {
                            releases.fetch_add(1, Ordering::SeqCst);
                        }
                        Ok(())
                    }
                },
            )
            .step(
                "step_c",
                move |_| {
                    let e = trigger_err_c.clone();
                    async move { Err(e) }
                },
                |_| async { Ok(()) },
            );

        let saga_id = "saga-post-comp-fail-1";
        let err = saga
            .run_with_id(saga_id, ())
            .await
            .err()
            .expect("post-compensation write must fail");
        assert_eq!(err.saga_id(), Some(saga_id));
        assert_eq!(b_releases.load(Ordering::SeqCst), 1);

        // Even with ordinary `resume` (simulating crash recovery), the stable compensation key
        // prevents releasing step_b's reservation twice.
        store.clear_failure();
        let report = saga
            .resume(saga_id, ())
            .await
            .expect("resume compensation must succeed");
        assert_eq!(
            report.status,
            SagaStatus::Compensated { error: trigger_err }
        );
        assert_eq!(
            b_releases.load(Ordering::SeqCst),
            1,
            "step_b undo must not be applied twice"
        );
        assert_eq!(a_releases.load(Ordering::SeqCst), 1);
    }
}
