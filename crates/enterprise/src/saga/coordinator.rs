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
        /// All compensation errors encountered during rollback, in execution order.
        #[serde(default)]
        compensation_errors: Vec<SagaError>,
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
    /// Monotonically increasing optimistic concurrency version for CAS updates.
    #[serde(default)]
    pub version: u64,
}

/// Storage contract for persisting saga execution records and state transitions.
pub trait SagaStore: Send + Sync + 'static {
    /// Atomically saves or updates a saga execution record if and only if the
    /// persisted record's `version` equals `expected_version` (`0` for a new record).
    ///
    /// Why: blind last-write-wins overwrites allow two concurrent recovery workers
    /// to interleave forward steps and compensations on the same `saga_id`.
    fn save_record_cas(
        &self,
        record: &SagaRecord,
        expected_version: u64,
    ) -> BoxFuture<'_, Result<(), SagaError>>;

    /// Loads a saga execution record by its unique ID.
    fn load_record(&self, saga_id: &str) -> BoxFuture<'_, Result<Option<SagaRecord>, SagaError>>;

    /// Lists all incomplete (non-terminal) saga executions for recovery.
    fn list_incomplete(&self) -> BoxFuture<'_, Result<Vec<SagaRecord>, SagaError>>;

    /// Acquires an exclusive execution claim for `saga_id`, held across forward step
    /// and compensation callbacks so two concurrent workers cannot interleave between
    /// individual CAS writes.
    ///
    /// Returns an opaque claim token on success, or [`SagaError::Storage`] if another
    /// worker currently owns execution for `saga_id`.
    fn try_acquire_execution(&self, saga_id: &str) -> BoxFuture<'_, Result<String, SagaError>> {
        let _ = saga_id;
        Box::pin(async { Ok(String::new()) })
    }

    /// Releases a previously acquired execution claim for `saga_id`.
    fn release_execution(
        &self,
        saga_id: &str,
        claim_token: &str,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        let _ = (saga_id, claim_token);
        Box::pin(async { Ok(()) })
    }

    /// Synchronously releases a previously acquired execution claim if the store
    /// supports lock-free / non-async teardown (e.g. in-memory stores).
    ///
    /// Returns `true` if the claim was released synchronously; `false` instructs
    /// `ExecutionClaimGuard::drop` to fall back to spawning [`Self::release_execution`].
    fn release_execution_sync(&self, saga_id: &str, claim_token: &str) -> bool {
        let _ = (saga_id, claim_token);
        false
    }
}

/// Thread-safe in-memory saga storage suitable for testing and single-instance deployments.
#[derive(Debug, Clone, Default)]
pub struct InMemorySagaStore {
    records: Arc<RwLock<HashMap<String, SagaRecord>>>,
    active_claims: Arc<std::sync::Mutex<HashMap<String, String>>>,
    max_retained_terminal: Option<usize>,
}

impl InMemorySagaStore {
    /// Creates a new in-memory saga store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Caps the number of terminal saga records retained in memory, evicting
    /// the oldest terminal records when the limit is exceeded.
    pub fn with_max_retained_terminal(mut self, max: usize) -> Self {
        self.max_retained_terminal = Some(max);
        self
    }

    /// Prunes terminal saga records beyond `max_retained` (keeping the newest by
    /// `updated_at`), returning the number of evicted records.
    pub async fn prune_terminal(&self, max_retained: usize) -> usize {
        let mut map = self.records.write().await;
        Self::prune_terminal_locked(&mut map, max_retained)
    }

    fn prune_terminal_locked(map: &mut HashMap<String, SagaRecord>, max_retained: usize) -> usize {
        let mut terminal: Vec<(String, u64)> = map
            .iter()
            .filter(|(_, r)| r.status.is_terminal())
            .map(|(k, r)| (k.clone(), r.updated_at))
            .collect();
        if terminal.len() <= max_retained {
            return 0;
        }
        // Evict oldest terminal records first.
        terminal.sort_by_key(|(_, ts)| *ts);
        let to_remove = terminal.len() - max_retained;
        for (id, _) in terminal.into_iter().take(to_remove) {
            map.remove(&id);
        }
        to_remove
    }
}

impl SagaStore for InMemorySagaStore {
    fn save_record_cas(
        &self,
        record: &SagaRecord,
        expected_version: u64,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        let record = record.clone();
        let records = Arc::clone(&self.records);
        let max_retained = self.max_retained_terminal;
        Box::pin(async move {
            let mut map = records.write().await;
            let current_version = map.get(&record.saga_id).map(|r| r.version).unwrap_or(0);
            if current_version != expected_version {
                return Err(SagaError::Storage {
                    saga_id: Some(record.saga_id.clone()),
                    message: format!(
                        "saga version conflict: expected version {expected_version}, found {current_version}"
                    ),
                });
            }
            let is_terminal = record.status.is_terminal();
            map.insert(record.saga_id.clone(), record);
            if is_terminal && let Some(max) = max_retained {
                Self::prune_terminal_locked(&mut map, max);
            }
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

    fn try_acquire_execution(&self, saga_id: &str) -> BoxFuture<'_, Result<String, SagaError>> {
        let id = saga_id.to_string();
        let active_claims = Arc::clone(&self.active_claims);
        Box::pin(async move {
            let mut claims = active_claims.lock().map_err(|_| SagaError::Storage {
                saga_id: Some(id.clone()),
                message: "saga claim lock poisoned".into(),
            })?;
            if claims.contains_key(&id) {
                return Err(SagaError::Storage {
                    saga_id: Some(id),
                    message: "saga execution already claimed by another worker".into(),
                });
            }
            let token = uuid::Uuid::new_v4().to_string();
            claims.insert(id, token.clone());
            Ok(token)
        })
    }

    fn release_execution(
        &self,
        saga_id: &str,
        claim_token: &str,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        self.release_execution_sync(saga_id, claim_token);
        Box::pin(async { Ok(()) })
    }

    fn release_execution_sync(&self, saga_id: &str, claim_token: &str) -> bool {
        if let Ok(mut claims) = self.active_claims.lock()
            && claims.get(saga_id).is_some_and(|t| t == claim_token)
        {
            claims.remove(saga_id);
        }
        true
    }
}

/// RAII guard that holds an exclusive per-saga execution claim across step and
/// compensation callbacks, releasing it on normal completion or task cancellation.
struct ExecutionClaimGuard {
    store: Option<Arc<dyn SagaStore>>,
    saga_id: String,
    token: String,
}

impl ExecutionClaimGuard {
    async fn release(mut self) {
        if let Some(store) = self.store.take() {
            let _ = store.release_execution(&self.saga_id, &self.token).await;
        }
    }
}

impl Drop for ExecutionClaimGuard {
    fn drop(&mut self) {
        if let Some(store) = self.store.take() {
            // Why: release synchronously when supported (e.g. `InMemorySagaStore`) so
            // dropping a pending saga future outside a Tokio runtime context cannot leak
            // the exclusive claim and permanently lock out recovery.
            if store.release_execution_sync(&self.saga_id, &self.token) {
                return;
            }
            let saga_id = self.saga_id.clone();
            let token = self.token.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = store.release_execution(&saga_id, &token).await;
                });
            }
        }
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
        /// Compensation errors encountered so far, in order.
        compensation_errors: Vec<SagaError>,
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
    /// Workflow name owning this saga instance (validated on [`Saga::resume_from_handle`]).
    pub saga_name: String,
    /// Mutated context preserved at the point of failure.
    pub context: Ctx,
    /// Phase the saga was in when the error occurred.
    pub phase: SagaRecoveryPhase,
    /// Step names that completed forward execution.
    pub completed_steps: Vec<String>,
    /// Step names that completed compensation.
    pub compensated_steps: Vec<String>,
    /// Last persisted CAS version observed before the failure.
    pub version: u64,
}

impl<Ctx> Debug for SagaRecoveryHandle<Ctx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SagaRecoveryHandle")
            .field("saga_id", &self.saga_id)
            .field("saga_name", &self.saga_name)
            .field("phase", &self.phase)
            .field("completed_steps", &self.completed_steps)
            .field("compensated_steps", &self.compensated_steps)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// Error returned by recoverable saga execution methods, pairing the [`SagaError`]
/// with a boxed [`SagaRecoveryHandle`] containing the mutated context and recovery state.
// Why: Boxing `handle` keeps `Result<SagaReport<Ctx>, SagaRecoveryError<Ctx>>` well below
// Clippy's `result_large_err` threshold (128 bytes) regardless of `Ctx` size.
pub struct SagaRecoveryError<Ctx> {
    /// Underlying saga error.
    pub error: SagaError,
    /// Recovery handle retaining `saga_id`, mutated `context`, and execution phase.
    pub handle: Box<SagaRecoveryHandle<Ctx>>,
}

impl<Ctx> SagaRecoveryError<Ctx> {
    /// Pairs a [`SagaError`] with a [`SagaRecoveryHandle`].
    pub fn new(error: SagaError, handle: SagaRecoveryHandle<Ctx>) -> Self {
        Self {
            error,
            handle: Box::new(handle),
        }
    }
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

    async fn acquire_execution_claim(
        &self,
        saga_id: &str,
    ) -> Result<Option<ExecutionClaimGuard>, SagaError> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let token = store
            .try_acquire_execution(saga_id)
            .await
            .map_err(|err| attach_saga_id(err, saga_id))?;
        Ok(Some(ExecutionClaimGuard {
            store: Some(Arc::clone(store)),
            saga_id: saga_id.to_string(),
            token,
        }))
    }

    async fn persist_state(
        &self,
        saga_id: &str,
        version: &mut u64,
        status: SagaStatus,
        completed_steps: &[String],
        compensated_steps: &[String],
    ) -> Result<(), SagaError> {
        if let Some(store) = &self.store {
            let expected_version = *version;
            let next_version = expected_version.saturating_add(1);
            let record = SagaRecord {
                saga_id: saga_id.to_string(),
                saga_name: self.name.clone(),
                status,
                completed_steps: completed_steps.to_vec(),
                compensated_steps: compensated_steps.to_vec(),
                updated_at: current_timestamp(),
                version: next_version,
            };
            // Why: CAS (`save_record_cas`) protects individual state transitions against
            // stale writes, while `acquire_execution_claim` holds an exclusive per-saga
            // claim across step and compensation callbacks so two workers cannot interleave.
            store
                .save_record_cas(&record, expected_version)
                .await
                .map_err(|err| attach_saga_id(err, saga_id))?;
            *version = next_version;
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
        let claim = match self.acquire_execution_claim(saga_id).await {
            Ok(c) => c,
            Err(error) => {
                return Err(SagaRecoveryError::new(
                    error,
                    SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        saga_name: self.name.clone(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Forward,
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                        version: 0,
                    },
                ));
            }
        };

        let mut version = 0u64;
        if let Err(error) = self
            .persist_state(saga_id, &mut version, SagaStatus::Pending, &[], &[])
            .await
        {
            if let Some(c) = claim {
                c.release().await;
            }
            return Err(SagaRecoveryError::new(
                error,
                SagaRecoveryHandle {
                    saga_id: saga_id.to_string(),
                    saga_name: self.name.clone(),
                    context: ctx,
                    phase: SagaRecoveryPhase::Forward,
                    completed_steps: Vec::new(),
                    compensated_steps: Vec::new(),
                    version,
                },
            ));
        }

        let res = self
            .execute_forward_from_recoverable(
                saga_id,
                version,
                ctx,
                0,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .await;
        if let Some(c) = claim {
            c.release().await;
        }
        res
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
                return Err(SagaRecoveryError::new(
                    SagaError::Storage {
                        saga_id: Some(saga_id.to_string()),
                        message: "cannot resume saga without a configured SagaStore".into(),
                    },
                    SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        saga_name: self.name.clone(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Forward,
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                        version: 0,
                    },
                ));
            }
        };

        let claim = match self.acquire_execution_claim(saga_id).await {
            Ok(c) => c,
            Err(error) => {
                return Err(SagaRecoveryError::new(
                    error,
                    SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        saga_name: self.name.clone(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Forward,
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                        version: 0,
                    },
                ));
            }
        };

        let record = match store.load_record(saga_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                if let Some(c) = claim {
                    c.release().await;
                }
                return Err(SagaRecoveryError::new(
                    SagaError::Storage {
                        saga_id: Some(saga_id.to_string()),
                        message: format!("saga record '{saga_id}' not found"),
                    },
                    SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        saga_name: self.name.clone(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Forward,
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                        version: 0,
                    },
                ));
            }
            Err(err) => {
                if let Some(c) = claim {
                    c.release().await;
                }
                return Err(SagaRecoveryError::new(
                    attach_saga_id(err, saga_id),
                    SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        saga_name: self.name.clone(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Forward,
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                        version: 0,
                    },
                ));
            }
        };

        if record.saga_name != self.name {
            if let Some(c) = claim {
                c.release().await;
            }
            return Err(SagaRecoveryError::new(
                SagaError::Aborted {
                    saga: self.name.clone(),
                    reason: format!(
                        "saga record '{saga_id}' belongs to '{}', not '{}'",
                        record.saga_name, self.name
                    ),
                },
                SagaRecoveryHandle {
                    saga_id: saga_id.to_string(),
                    saga_name: record.saga_name,
                    context: ctx,
                    phase: SagaRecoveryPhase::Forward,
                    completed_steps: record.completed_steps,
                    compensated_steps: record.compensated_steps,
                    version: record.version,
                },
            ));
        }

        let res = match record.status {
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
        };
        if let Some(c) = claim {
            c.release().await;
        }
        res
    }

    /// Resumes an in-process saga from a [`SagaRecoveryHandle`] returned by a previous
    /// [`SagaRecoveryError`], preserving the mutated `Ctx`, completed/compensated steps,
    /// and rollback intent.
    pub async fn resume_from_handle(
        &self,
        handle: SagaRecoveryHandle<Ctx>,
    ) -> Result<SagaReport<Ctx>, SagaRecoveryError<Ctx>> {
        // Why: reject handles originating from a different workflow even when step names
        // match, preventing cross-workflow hijacking and `saga_name` overwrites via CAS.
        if handle.saga_name != self.name {
            let reason = format!(
                "saga recovery handle '{}' belongs to '{}', not '{}'",
                handle.saga_id, handle.saga_name, self.name
            );
            return Err(SagaRecoveryError::new(
                SagaError::Aborted {
                    saga: self.name.clone(),
                    reason,
                },
                handle,
            ));
        }
        let completed_indices = match self.resolve_completed_indices(&handle.completed_steps) {
            Ok(indices) => indices,
            Err(err) => return Err(SagaRecoveryError::new(err, handle)),
        };
        let claim = match self.acquire_execution_claim(&handle.saga_id).await {
            Ok(c) => c,
            Err(err) => return Err(SagaRecoveryError::new(err, handle)),
        };

        // Why: validate the persisted record under the execution claim before dispatching
        // a recovery handle so a `Compensating` handle cannot roll back an already-`Success`
        // saga or overwrite an existing terminal outcome (`Compensated`/`CompensationFailed`).
        if let Some(store) = &self.store {
            match store.load_record(&handle.saga_id).await {
                Ok(Some(record)) => {
                    if record.saga_name != self.name {
                        if let Some(c) = claim {
                            c.release().await;
                        }
                        let reason = format!(
                            "saga record '{}' belongs to '{}', not '{}'",
                            handle.saga_id, record.saga_name, self.name
                        );
                        return Err(SagaRecoveryError::new(
                            SagaError::Aborted {
                                saga: self.name.clone(),
                                reason,
                            },
                            handle,
                        ));
                    }
                    match record.status {
                        SagaStatus::Success => {
                            if let Some(c) = claim {
                                c.release().await;
                            }
                            if matches!(handle.phase, SagaRecoveryPhase::Compensating { .. }) {
                                let reason = format!(
                                    "cannot compensate already succeeded saga '{}'",
                                    handle.saga_id
                                );
                                return Err(SagaRecoveryError::new(
                                    SagaError::Aborted {
                                        saga: self.name.clone(),
                                        reason,
                                    },
                                    handle,
                                ));
                            }
                            return Ok(SagaReport {
                                context: handle.context,
                                status: record.status,
                                completed_steps: record.completed_steps,
                                compensated_steps: record.compensated_steps,
                            });
                        }
                        SagaStatus::Compensated { .. } | SagaStatus::CompensationFailed { .. } => {
                            if let Some(c) = claim {
                                c.release().await;
                            }
                            return Ok(SagaReport {
                                context: handle.context,
                                status: record.status,
                                completed_steps: record.completed_steps,
                                compensated_steps: record.compensated_steps,
                            });
                        }
                        SagaStatus::Pending
                        | SagaStatus::Running { .. }
                        | SagaStatus::Compensating { .. } => {}
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    if let Some(c) = claim {
                        c.release().await;
                    }
                    let saga_id = handle.saga_id.clone();
                    return Err(SagaRecoveryError::new(
                        attach_saga_id(err, &saga_id),
                        handle,
                    ));
                }
            }
        }

        let res = match handle.phase {
            SagaRecoveryPhase::Forward => {
                let start_idx = completed_indices.last().map(|i| i + 1).unwrap_or(0);
                self.execute_forward_from_recoverable(
                    &handle.saga_id,
                    handle.version,
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
                compensation_errors,
            } => {
                self.compensate_completed_recoverable(
                    &handle.saga_id,
                    handle.version,
                    handle.context,
                    &completed_indices,
                    &handle.completed_steps,
                    handle.compensated_steps,
                    error,
                    compensation_errors,
                )
                .await
            }
        };
        if let Some(c) = claim {
            c.release().await;
        }
        res
    }

    fn resolve_completed_indices(
        &self,
        completed_names: &[String],
    ) -> Result<Vec<usize>, SagaError> {
        let mut indices = Vec::with_capacity(completed_names.len());
        for name in completed_names {
            if let Some((idx, _)) = self
                .steps
                .iter()
                .enumerate()
                .find(|(_, s)| s.name() == name)
            {
                indices.push(idx);
            } else {
                // Why: silently dropping unknown/renamed step names during recovery
                // would skip compensating them or restart forward execution from the
                // wrong index. Fail closed instead.
                return Err(SagaError::Aborted {
                    saga: self.name.clone(),
                    reason: format!(
                        "persisted step '{name}' does not exist in saga '{}'",
                        self.name
                    ),
                });
            }
        }
        Ok(indices)
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_forward_from_recoverable(
        &self,
        saga_id: &str,
        mut version: u64,
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
                    &mut version,
                    SagaStatus::Running {
                        current_step: step.name().to_string(),
                    },
                    &completed_names,
                    &compensated_names,
                )
                .await
            {
                return Err(SagaRecoveryError::new(
                    error,
                    SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        saga_name: self.name.clone(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Forward,
                        completed_steps: completed_names,
                        compensated_steps: compensated_names,
                        version,
                    },
                ));
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
            // Why: pass `parent_tc` directly to `scope_future(step_span, ...)` so `OpenTelemetryLayer`
            // links each `saga.step` directly to the initiating span across step boundaries (instead
            // of a phantom `new_child()` parent ID), while non-OTel runs keep `OtelContext::current()`
            // fallback populated inside step handlers.
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
                            &mut version,
                            SagaStatus::Running {
                                current_step: step.name().to_string(),
                            },
                            &completed_names,
                            &compensated_names,
                        )
                        .await
                    {
                        return Err(SagaRecoveryError::new(
                            error,
                            SagaRecoveryHandle {
                                saga_id: saga_id.to_string(),
                                saga_name: self.name.clone(),
                                context: ctx,
                                phase: SagaRecoveryPhase::Forward,
                                completed_steps: completed_names,
                                compensated_steps: compensated_names,
                                version,
                            },
                        ));
                    }
                }
                Err(err) => {
                    return self
                        .compensate_completed_recoverable(
                            saga_id,
                            version,
                            ctx,
                            &completed_indices,
                            &completed_names,
                            compensated_names,
                            Some(err),
                            Vec::new(),
                        )
                        .await;
                }
            }
        }

        if let Err(error) = self
            .persist_state(
                saga_id,
                &mut version,
                SagaStatus::Success,
                &completed_names,
                &compensated_names,
            )
            .await
        {
            return Err(SagaRecoveryError::new(
                error,
                SagaRecoveryHandle {
                    saga_id: saga_id.to_string(),
                    saga_name: self.name.clone(),
                    context: ctx,
                    phase: SagaRecoveryPhase::Forward,
                    completed_steps: completed_names,
                    compensated_steps: compensated_names,
                    version,
                },
            ));
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
        mut version: u64,
        mut ctx: Ctx,
        completed_indices: &[usize],
        completed_names: &[String],
        mut compensated_names: Vec<String>,
        original_error: Option<SagaError>,
        mut compensation_errors: Vec<SagaError>,
    ) -> Result<SagaReport<Ctx>, SagaRecoveryError<Ctx>> {
        let parent_tc = rume_observability::TraceContext::current();
        for &prev_idx in completed_indices.iter().rev() {
            let prev_step = &self.steps[prev_idx];
            if compensated_names.iter().any(|n| n == prev_step.name()) {
                continue;
            }

            if let Err(error) = self
                .persist_state(
                    saga_id,
                    &mut version,
                    SagaStatus::Compensating {
                        current_step: prev_step.name().to_string(),
                        error: original_error.clone(),
                    },
                    completed_names,
                    &compensated_names,
                )
                .await
            {
                return Err(SagaRecoveryError::new(
                    error,
                    SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        saga_name: self.name.clone(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Compensating {
                            error: original_error,
                            compensation_errors,
                        },
                        completed_steps: completed_names.to_vec(),
                        compensated_steps: compensated_names,
                        version,
                    },
                ));
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
            // Why: pass `parent_tc` directly to `scope_future(comp_span, ...)` so `OpenTelemetryLayer`
            // links each `saga.compensate` directly to the initiating trace across compensation
            // boundaries, while non-OTel runs preserve `OtelContext::current()` fallback.
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
                            &mut version,
                            SagaStatus::Compensating {
                                current_step: prev_step.name().to_string(),
                                error: original_error.clone(),
                            },
                            completed_names,
                            &compensated_names,
                        )
                        .await
                    {
                        return Err(SagaRecoveryError::new(
                            error,
                            SagaRecoveryHandle {
                                saga_id: saga_id.to_string(),
                                saga_name: self.name.clone(),
                                context: ctx,
                                phase: SagaRecoveryPhase::Compensating {
                                    error: original_error,
                                    compensation_errors,
                                },
                                completed_steps: completed_names.to_vec(),
                                compensated_steps: compensated_names,
                                version,
                            },
                        ));
                    }
                }
                Err(comp_err) => {
                    tracing::error!(
                        saga.name = %self.name,
                        saga.id = %saga_id,
                        saga.step = %prev_step.name(),
                        error = %comp_err,
                        "Saga step compensation failed"
                    );
                    compensation_errors.push(comp_err);
                }
            }
        }

        let resolved_error = original_error.unwrap_or_else(|| SagaError::Aborted {
            saga: self.name.clone(),
            reason: if !compensation_errors.is_empty() {
                "resumed compensation encountered failure".into()
            } else {
                "resumed compensation completed successfully".into()
            },
        });
        let status = if let Some(first_comp_error) = compensation_errors.first().cloned() {
            SagaStatus::CompensationFailed {
                error: resolved_error.clone(),
                compensation_error: first_comp_error,
                compensation_errors: compensation_errors.clone(),
            }
        } else {
            SagaStatus::Compensated {
                error: resolved_error.clone(),
            }
        };

        if let Err(error) = self
            .persist_state(
                saga_id,
                &mut version,
                status.clone(),
                completed_names,
                &compensated_names,
            )
            .await
        {
            return Err(SagaRecoveryError::new(
                error,
                SagaRecoveryHandle {
                    saga_id: saga_id.to_string(),
                    saga_name: self.name.clone(),
                    context: ctx,
                    phase: SagaRecoveryPhase::Compensating {
                        error: Some(resolved_error),
                        compensation_errors,
                    },
                    completed_steps: completed_names.to_vec(),
                    compensated_steps: compensated_names,
                    version,
                },
            ));
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
        let completed_indices = match self.resolve_completed_indices(&record.completed_steps) {
            Ok(indices) => indices,
            Err(err) => {
                return Err(SagaRecoveryError::new(
                    err,
                    SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        saga_name: record.saga_name,
                        context: ctx,
                        phase: SagaRecoveryPhase::Forward,
                        completed_steps: record.completed_steps,
                        compensated_steps: record.compensated_steps,
                        version: record.version,
                    },
                ));
            }
        };
        let start_idx = completed_indices.last().map(|i| i + 1).unwrap_or(0);

        self.execute_forward_from_recoverable(
            saga_id,
            record.version,
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
        let original_error = match record.status {
            SagaStatus::Compensating { error, .. } => error,
            _ => None,
        };
        let completed_indices = match self.resolve_completed_indices(&record.completed_steps) {
            Ok(indices) => indices,
            Err(err) => {
                return Err(SagaRecoveryError::new(
                    err,
                    SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        saga_name: record.saga_name,
                        context: ctx,
                        phase: SagaRecoveryPhase::Compensating {
                            error: original_error,
                            compensation_errors: Vec::new(),
                        },
                        completed_steps: record.completed_steps,
                        compensated_steps: record.compensated_steps,
                        version: record.version,
                    },
                ));
            }
        };

        self.compensate_completed_recoverable(
            saga_id,
            record.version,
            ctx,
            &completed_indices,
            &record.completed_steps,
            record.compensated_steps,
            original_error,
            Vec::new(),
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
                return Err(SagaRecoveryError::new(
                    SagaError::Storage {
                        saga_id: Some(saga_id.to_string()),
                        message: "cannot compensate crashed saga without a configured SagaStore"
                            .into(),
                    },
                    SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        saga_name: self.name.clone(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Compensating {
                            error: None,
                            compensation_errors: Vec::new(),
                        },
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                        version: 0,
                    },
                ));
            }
        };

        let claim = match self.acquire_execution_claim(saga_id).await {
            Ok(c) => c,
            Err(error) => {
                return Err(SagaRecoveryError::new(
                    error,
                    SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        saga_name: self.name.clone(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Compensating {
                            error: None,
                            compensation_errors: Vec::new(),
                        },
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                        version: 0,
                    },
                ));
            }
        };

        let record = match store.load_record(saga_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                if let Some(c) = claim {
                    c.release().await;
                }
                return Err(SagaRecoveryError::new(
                    SagaError::Storage {
                        saga_id: Some(saga_id.to_string()),
                        message: format!("saga record '{saga_id}' not found"),
                    },
                    SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        saga_name: self.name.clone(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Compensating {
                            error: None,
                            compensation_errors: Vec::new(),
                        },
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                        version: 0,
                    },
                ));
            }
            Err(err) => {
                if let Some(c) = claim {
                    c.release().await;
                }
                return Err(SagaRecoveryError::new(
                    attach_saga_id(err, saga_id),
                    SagaRecoveryHandle {
                        saga_id: saga_id.to_string(),
                        saga_name: self.name.clone(),
                        context: ctx,
                        phase: SagaRecoveryPhase::Compensating {
                            error: None,
                            compensation_errors: Vec::new(),
                        },
                        completed_steps: Vec::new(),
                        compensated_steps: Vec::new(),
                        version: 0,
                    },
                ));
            }
        };

        if record.saga_name != self.name {
            if let Some(c) = claim {
                c.release().await;
            }
            return Err(SagaRecoveryError::new(
                SagaError::Aborted {
                    saga: self.name.clone(),
                    reason: format!(
                        "saga record '{saga_id}' belongs to '{}', not '{}'",
                        record.saga_name, self.name
                    ),
                },
                SagaRecoveryHandle {
                    saga_id: saga_id.to_string(),
                    saga_name: record.saga_name,
                    context: ctx,
                    phase: SagaRecoveryPhase::Compensating {
                        error: None,
                        compensation_errors: Vec::new(),
                    },
                    completed_steps: record.completed_steps,
                    compensated_steps: record.compensated_steps,
                    version: record.version,
                },
            ));
        }

        // Why: terminal sagas must never be rolled back or overwritten. A `Success`
        // saga already committed; already-`Compensated` / `CompensationFailed`
        // sagas are returned intact to preserve their original error record.
        let res = match record.status {
            SagaStatus::Success => Err(SagaRecoveryError::new(
                SagaError::Aborted {
                    saga: self.name.clone(),
                    reason: format!("cannot compensate already succeeded saga '{saga_id}'"),
                },
                SagaRecoveryHandle {
                    saga_id: saga_id.to_string(),
                    saga_name: record.saga_name,
                    context: ctx,
                    phase: SagaRecoveryPhase::Forward,
                    completed_steps: record.completed_steps,
                    compensated_steps: record.compensated_steps,
                    version: record.version,
                },
            )),
            SagaStatus::Compensated { .. } | SagaStatus::CompensationFailed { .. } => {
                Ok(SagaReport {
                    context: ctx,
                    status: record.status,
                    completed_steps: record.completed_steps,
                    compensated_steps: record.compensated_steps,
                })
            }
            SagaStatus::Pending | SagaStatus::Running { .. } | SagaStatus::Compensating { .. } => {
                self.resume_compensation_recoverable(saga_id, record, ctx)
                    .await
            }
        };
        if let Some(c) = claim {
            c.release().await;
        }
        res
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
        fn save_record_cas(
            &self,
            record: &SagaRecord,
            expected_version: u64,
        ) -> BoxFuture<'_, Result<(), SagaError>> {
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
            self.inner.save_record_cas(record, expected_version)
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
            .resume_from_handle(*err.handle)
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
                ref compensation_errors,
            } if *e == original_err && compensation_errors.is_empty()
        ));

        store.clear_failure();
        let report = saga
            .resume_from_handle(*err.handle)
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

    #[tokio::test]
    async fn compensate_crashed_rejects_success_and_preserves_compensated() {
        let store = Arc::new(InMemorySagaStore::new());
        let comp_runs = Arc::new(AtomicUsize::new(0));
        let comp_runs_c = Arc::clone(&comp_runs);

        let saga = Saga::<()>::new("order").with_store(store.clone()).step(
            "reserve",
            |_| async { Ok(()) },
            move |_| {
                comp_runs_c.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            },
        );

        saga.run_with_id("s-ok", ()).await.unwrap();
        let err = saga.compensate_crashed("s-ok", ()).await.err().unwrap();
        assert!(matches!(err, SagaError::Aborted { .. }));
        assert_eq!(comp_runs.load(Ordering::SeqCst), 0);
        assert_eq!(
            store.load_record("s-ok").await.unwrap().unwrap().status,
            SagaStatus::Success
        );
    }

    #[tokio::test]
    async fn resume_rejects_mismatched_saga_name_and_unknown_step() {
        let store = Arc::new(InMemorySagaStore::new());
        store
            .save_record_cas(
                &SagaRecord {
                    saga_id: "s-other".into(),
                    saga_name: "other_saga".into(),
                    status: SagaStatus::Running {
                        current_step: "step_1".into(),
                    },
                    completed_steps: vec!["step_1".into()],
                    compensated_steps: vec![],
                    updated_at: 1,
                    version: 1,
                },
                0,
            )
            .await
            .unwrap();

        let saga = Saga::<()>::new("order").with_store(store.clone()).step(
            "step_1",
            |_| async { Ok(()) },
            |_| async { Ok(()) },
        );
        let err = saga.resume("s-other", ()).await.err().unwrap();
        assert!(matches!(err, SagaError::Aborted { .. }));

        store
            .save_record_cas(
                &SagaRecord {
                    saga_id: "s-bad-step".into(),
                    saga_name: "order".into(),
                    status: SagaStatus::Running {
                        current_step: "ghost_step".into(),
                    },
                    completed_steps: vec!["ghost_step".into()],
                    compensated_steps: vec![],
                    updated_at: 1,
                    version: 1,
                },
                0,
            )
            .await
            .unwrap();
        let err2 = saga.resume("s-bad-step", ()).await.err().unwrap();
        assert!(matches!(err2, SagaError::Aborted { .. }));
    }

    #[tokio::test]
    async fn concurrent_resume_cas_allows_single_winner_and_retains_all_comp_errors() {
        let store = Arc::new(InMemorySagaStore::new().with_max_retained_terminal(1));
        let saga = Arc::new(
            Saga::<()>::new("dual_comp")
                .with_store(store.clone())
                .step(
                    "s1",
                    |_| async { Ok(()) },
                    |_| async {
                        Err(SagaError::CompensationFailed {
                            step: "s1".into(),
                            message: "c1".into(),
                        })
                    },
                )
                .step(
                    "s2",
                    |_| async { Ok(()) },
                    |_| async {
                        Err(SagaError::CompensationFailed {
                            step: "s2".into(),
                            message: "c2".into(),
                        })
                    },
                )
                .step(
                    "s3",
                    |_| async {
                        Err(SagaError::ActionFailed {
                            step: "s3".into(),
                            message: "boom".into(),
                        })
                    },
                    |_| async { Ok(()) },
                ),
        );

        let report = saga.run_with_id("s-dual", ()).await.unwrap();
        match report.status {
            SagaStatus::CompensationFailed {
                compensation_errors,
                ..
            } => assert_eq!(compensation_errors.len(), 2),
            other => panic!("expected CompensationFailed, got {other:?}"),
        }

        // Second terminal saga triggers max_retained_terminal(1) pruning.
        let _ = saga.run_with_id("s-dual-2", ()).await.unwrap();
        assert_eq!(store.records.read().await.len(), 1);
    }

    #[tokio::test]
    async fn overlapping_resume_is_blocked_by_exclusive_execution_claim() {
        let store = Arc::new(InMemorySagaStore::new());
        store
            .save_record_cas(
                &SagaRecord {
                    saga_id: "s-overlap".into(),
                    saga_name: "overlap_saga".into(),
                    status: SagaStatus::Running {
                        current_step: "step_2".into(),
                    },
                    completed_steps: vec!["step_1".into()],
                    compensated_steps: vec![],
                    updated_at: 1,
                    version: 1,
                },
                0,
            )
            .await
            .unwrap();

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let step2_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let e_clone = Arc::clone(&entered);
        let r_clone = Arc::clone(&release);
        let runs_clone = Arc::clone(&step2_runs);

        let saga = Arc::new(
            Saga::<()>::new("overlap_saga")
                .with_store(store.clone())
                .step("step_1", |_| async { Ok(()) }, |_| async { Ok(()) })
                .step(
                    "step_2",
                    move |_| {
                        let entered = Arc::clone(&e_clone);
                        let release = Arc::clone(&r_clone);
                        let runs = Arc::clone(&runs_clone);
                        async move {
                            runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            entered.notify_one();
                            release.notified().await;
                            Ok(())
                        }
                    },
                    |_| async { Ok(()) },
                ),
        );

        let worker_a_saga = Arc::clone(&saga);
        let worker_a = tokio::spawn(async move { worker_a_saga.resume("s-overlap", ()).await });
        entered.notified().await;

        // While Worker A is paused inside `step_2`, Worker B must be rejected before entering the step.
        let worker_b_err = saga.resume("s-overlap", ()).await.err().unwrap();
        assert!(matches!(worker_b_err, SagaError::Storage { .. }));
        assert_eq!(step2_runs.load(std::sync::atomic::Ordering::SeqCst), 1);

        release.notify_one();
        let report = worker_a.await.unwrap().expect("worker A succeeds");
        assert_eq!(report.status, SagaStatus::Success);
        assert_eq!(step2_runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resume_from_handle_rejects_mismatched_saga_name_handle() {
        let store = Arc::new(InMemorySagaStore::new());
        store
            .save_record_cas(
                &SagaRecord {
                    saga_id: "s-cross".into(),
                    saga_name: "workflow_a".into(),
                    status: SagaStatus::Running {
                        current_step: "step_2".into(),
                    },
                    completed_steps: vec!["step_1".into()],
                    compensated_steps: vec![],
                    updated_at: 1,
                    version: 1,
                },
                0,
            )
            .await
            .unwrap();

        let workflow_b = Saga::<()>::new("workflow_b")
            .with_store(store.clone())
            .step("step_1", |_| async { Ok(()) }, |_| async { Ok(()) })
            .step("step_2", |_| async { Ok(()) }, |_| async { Ok(()) });

        let err = workflow_b
            .resume_recoverable("s-cross", ())
            .await
            .err()
            .unwrap();
        assert!(matches!(err.error, SagaError::Aborted { .. }));

        // Passing the rejected handle back into `workflow_b.resume_from_handle` must also fail
        // and must NOT overwrite `workflow_a`'s stored record.
        let err2 = workflow_b
            .resume_from_handle(*err.handle)
            .await
            .err()
            .unwrap();
        assert!(matches!(err2.error, SagaError::Aborted { .. }));
        let stored = store.load_record("s-cross").await.unwrap().unwrap();
        assert_eq!(stored.saga_name, "workflow_a");
        assert_eq!(stored.version, 1);
    }

    #[test]
    fn dropping_pending_saga_future_outside_runtime_releases_in_memory_claim() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store = Arc::new(InMemorySagaStore::new());
        let saga = Saga::<()>::new("drop_outside_rt")
            .with_store(store.clone())
            .step(
                "s1",
                |_| async {
                    std::future::pending::<()>().await;
                    Ok(())
                },
                |_| async { Ok(()) },
            );

        // Poll the saga future inside the runtime context so it acquires the execution claim
        // and parks inside `s1`, then leave the runtime context before dropping the future.
        let mut fut = Box::pin(saga.run_with_id("s-drop-sync", ()));
        {
            let _enter = rt.enter();
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(fut.as_mut().poll(&mut cx).is_pending());
        }
        // Drop outside any Tokio runtime context; synchronous release must clear the claim.
        drop(fut);

        // A subsequent recovery on the same saga_id must succeed without a stuck claim error.
        let saga_fast = Saga::<()>::new("drop_outside_rt").with_store(store).step(
            "s1",
            |_| async { Ok(()) },
            |_| async { Ok(()) },
        );
        let report = rt
            .block_on(saga_fast.resume("s-drop-sync", ()))
            .expect("claim must be released when future is dropped outside runtime");
        assert_eq!(report.status, SagaStatus::Success);
    }

    #[tokio::test]
    async fn resume_from_handle_rejects_compensating_already_succeeded_saga() {
        let store = Arc::new(InMemorySagaStore::new());
        let comp_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let comp_clone = Arc::clone(&comp_runs);
        let workflow_a = Saga::<()>::new("workflow_a")
            .with_store(store.clone())
            .step(
                "step_1",
                |_| async { Ok(()) },
                move |_| {
                    comp_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    async { Ok(()) }
                },
            );

        let report = workflow_a.run_with_id("s-succeeded", ()).await.unwrap();
        assert_eq!(report.status, SagaStatus::Success);

        // Workflow B calls `compensate_crashed_recoverable` on workflow A's succeeded saga,
        // yielding a handle carrying workflow A's name, current version, and Compensating phase.
        let workflow_b = Saga::<()>::new("workflow_b")
            .with_store(store.clone())
            .step("step_1", |_| async { Ok(()) }, |_| async { Ok(()) });
        let mismatch_err = workflow_b
            .compensate_crashed_recoverable("s-succeeded", ())
            .await
            .err()
            .unwrap();
        assert_eq!(mismatch_err.handle.saga_name, "workflow_a");

        // Passing that handle to workflow A's `resume_from_handle` must reject compensation
        // and preserve `SagaStatus::Success`.
        let err = workflow_a
            .resume_from_handle(*mismatch_err.handle)
            .await
            .err()
            .unwrap();
        assert!(matches!(err.error, SagaError::Aborted { .. }));
        assert_eq!(comp_runs.load(std::sync::atomic::Ordering::SeqCst), 0);
        let stored = store.load_record("s-succeeded").await.unwrap().unwrap();
        assert_eq!(stored.status, SagaStatus::Success);
    }
}
