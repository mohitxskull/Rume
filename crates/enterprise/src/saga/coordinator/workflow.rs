//! Saga workflow runner: execution, recovery and compensation.

use std::fmt::Debug;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt;

use super::store::{ExecutionClaimGuard, InMemorySagaStore, SagaRecord, SagaStatus, SagaStore};
use crate::saga::error::SagaError;
use crate::saga::step::{FunctionalStep, Step};

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Builds a deterministic forward-execution idempotency key for a `(saga_id, step_name)` pair.
pub fn forward_idempotency_key(saga_id: &str, step_name: &str) -> String {
    format!("saga:{saga_id}:forward:{step_name}")
}

/// Builds a deterministic compensation idempotency key for a `(saga_id, step_name)` pair.
pub fn compensation_idempotency_key(saga_id: &str, step_name: &str) -> String {
    format!("saga:{saga_id}:compensate:{step_name}")
}

fn merge_step_lists(authoritative: &[String], handle_steps: &[String]) -> Vec<String> {
    let mut merged = authoritative.to_vec();
    for step in handle_steps {
        if !merged.iter().any(|existing| existing == step) {
            merged.push(step.clone());
        }
    }
    merged
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

    fn spawn_claim_guard(
        store: &Arc<dyn SagaStore>,
        saga_id: &str,
        token: String,
    ) -> ExecutionClaimGuard {
        let (claim_lost_tx, claim_lost_rx) = tokio::sync::watch::channel(false);
        let renew_task = store.claim_ttl().map(|ttl| {
            let store = Arc::clone(store);
            let saga_id = saga_id.to_string();
            let token = token.clone();
            let interval = (ttl / 3).max(std::time::Duration::from_millis(50));
            // Why: Renew lease-backed execution claims periodically while long-running steps
            // execute (>60s) so the active worker stays fenced and another worker cannot
            // steal the lease mid-step. On explicit claim loss (`Ok(false)`), signal
            // `claim_lost_tx` so in-flight step execution cancels immediately (`RC-23`).
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                // Consume the immediate first tick so we wait one interval before renewing.
                ticker.tick().await;
                loop {
                    ticker.tick().await;
                    match store.renew_execution(&saga_id, &token).await {
                        Ok(true) => {}
                        Ok(false) => {
                            let _ = claim_lost_tx.send(true);
                            tracing::warn!(
                                saga_id = %saga_id,
                                "saga execution claim lost or expired; cancelling in-flight step and stopping renewal loop"
                            );
                            break;
                        }
                        Err(err) => {
                            // Why: a single transient DB/Redis blip must not permanently abort
                            // lease renewal while the step is still running; retry on the next
                            // tick and only break on explicit claim loss (`Ok(false)`) or guard
                            // cancellation.
                            tracing::warn!(
                                saga_id = %saga_id,
                                error = %err,
                                "transient error renewing saga execution claim; retrying on next tick"
                            );
                        }
                    }
                }
            })
        });
        ExecutionClaimGuard {
            store: Some(Arc::clone(store)),
            saga_id: saga_id.to_string(),
            token,
            renew_task,
            claim_lost_rx,
        }
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
        Ok(Some(Self::spawn_claim_guard(store, saga_id, token)))
    }

    async fn persist_state(
        &self,
        saga_id: &str,
        version: &mut u64,
        claim_token: Option<&str>,
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
            // Why: CAS (`save_record_cas_with_claim`) protects individual state transitions
            // against both stale versions and stale claim tokens if another worker took over
            // after a lease expiry.
            store
                .save_record_cas_with_claim(&record, expected_version, claim_token)
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
        // Why: atomically insert the initial `Pending` row AND acquire its execution claim
        // in one operation so no concurrent recovery worker can observe a `claim_token = NULL`
        // window between `Pending` insertion and claim acquisition (`RC-09`).
        let (version, claim) = if let Some(store) = &self.store {
            let record = SagaRecord {
                saga_id: saga_id.to_string(),
                saga_name: self.name.clone(),
                status: SagaStatus::Pending,
                completed_steps: Vec::new(),
                compensated_steps: Vec::new(),
                updated_at: current_timestamp(),
                version: 1,
            };
            match store.create_and_claim_record(&record).await {
                Ok(token) => (1u64, Some(Self::spawn_claim_guard(store, saga_id, token))),
                Err(error) => {
                    return Err(SagaRecoveryError::new(
                        attach_saga_id(error, saga_id),
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
            }
        } else {
            (0u64, None)
        };

        let claim_token = claim.as_ref().map(|c| c.token.as_str());
        let claim_lost = claim.as_ref().map(|c| &c.claim_lost_rx);
        let res = self
            .execute_forward_from_recoverable(
                saga_id,
                version,
                claim_token,
                claim_lost,
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

        let claim_token = claim.as_ref().map(|c| c.token.as_str());
        let claim_lost = claim.as_ref().map(|c| &c.claim_lost_rx);
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
                self.resume_compensation_recoverable(saga_id, claim_token, claim_lost, record, ctx)
                    .await
            }
            SagaStatus::Pending | SagaStatus::Running { .. } => {
                self.resume_forward_recoverable(saga_id, claim_token, claim_lost, record, ctx)
                    .await
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
        if handle.version == 0 && matches!(handle.phase, SagaRecoveryPhase::Forward) {
            // Why: When the initial `persist_state(Pending)` failed before any row existed
            // (`version == 0`), delegate to `run_with_id_recoverable` so the initial row is
            // created. However, a `version == 0 && Forward` handle can also come from
            // contention against an ALREADY-persisted saga (`P2-05`); check `load_record`
            // first and delegate ONLY when no record exists, otherwise fall through into
            // claim + authoritative reconciliation.
            let should_create = match &self.store {
                None => true,
                Some(store) => match store.load_record(&handle.saga_id).await {
                    Ok(None) => true,
                    Ok(Some(_)) => false,
                    Err(err) => {
                        let saga_id = handle.saga_id.clone();
                        return Err(SagaRecoveryError::new(
                            attach_saga_id(err, &saga_id),
                            handle,
                        ));
                    }
                },
            };
            if should_create {
                return self
                    .run_with_id_recoverable(&handle.saga_id, handle.context)
                    .await;
            }
        }
        if let Err(err) = self.resolve_completed_indices(&handle.completed_steps) {
            return Err(SagaRecoveryError::new(err, handle));
        }
        let claim = match self.acquire_execution_claim(&handle.saga_id).await {
            Ok(c) => c,
            Err(err) => return Err(SagaRecoveryError::new(err, handle)),
        };

        let mut effective_version = handle.version;
        let mut effective_completed_steps = handle.completed_steps.clone();
        let mut effective_compensated_steps = handle.compensated_steps.clone();
        let mut reconciled_trigger_error: Option<SagaError> = None;

        // Why: validate AND reconcile the persisted record under the execution claim before
        // dispatching a recovery handle so a `Compensating` handle cannot roll back an
        // already-`Success` saga, overwrite an existing terminal outcome, or loop on a stale
        // `handle.version` / `completed_steps` after contention (`RC-08`).
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
                        SagaStatus::Compensating { error: rec_err, .. } => {
                            // Why: once a saga has persisted `Compensating`, a `Forward` recovery
                            // handle (e.g. from a cross-workflow `resume_recoverable` mismatch)
                            // must never resume forward execution after steps may already be undone.
                            if matches!(handle.phase, SagaRecoveryPhase::Forward) {
                                if let Some(c) = claim {
                                    c.release().await;
                                }
                                let reason = format!(
                                    "cannot resume forward execution for compensating saga '{}'",
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
                            reconciled_trigger_error = rec_err;
                        }
                        SagaStatus::Pending | SagaStatus::Running { .. } => {}
                    }

                    effective_version = record.version;
                    effective_completed_steps =
                        merge_step_lists(&record.completed_steps, &handle.completed_steps);
                    effective_compensated_steps =
                        merge_step_lists(&record.compensated_steps, &handle.compensated_steps);
                }
                Ok(None) => {
                    if handle.version > 0
                        || matches!(handle.phase, SagaRecoveryPhase::Compensating { .. })
                    {
                        if let Some(c) = claim {
                            c.release().await;
                        }
                        let saga_id = handle.saga_id.clone();
                        return Err(SagaRecoveryError::new(
                            SagaError::Storage {
                                saga_id: Some(saga_id.clone()),
                                message: format!("saga record '{saga_id}' not found"),
                            },
                            handle,
                        ));
                    }
                }
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

        let completed_indices = match self.resolve_completed_indices(&effective_completed_steps) {
            Ok(indices) => indices,
            Err(err) => {
                if let Some(c) = claim {
                    c.release().await;
                }
                return Err(SagaRecoveryError::new(err, handle));
            }
        };

        let claim_token = claim.as_ref().map(|c| c.token.as_str());
        let claim_lost = claim.as_ref().map(|c| &c.claim_lost_rx);
        let res = match handle.phase {
            SagaRecoveryPhase::Forward => {
                let start_idx = completed_indices.last().map(|i| i + 1).unwrap_or(0);
                self.execute_forward_from_recoverable(
                    &handle.saga_id,
                    effective_version,
                    claim_token,
                    claim_lost,
                    handle.context,
                    start_idx,
                    completed_indices,
                    effective_completed_steps,
                    effective_compensated_steps,
                )
                .await
            }
            SagaRecoveryPhase::Compensating {
                error,
                compensation_errors,
            } => {
                self.compensate_completed_recoverable(
                    &handle.saga_id,
                    effective_version,
                    claim_token,
                    claim_lost,
                    handle.context,
                    &completed_indices,
                    &effective_completed_steps,
                    effective_compensated_steps,
                    error.or(reconciled_trigger_error),
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
        claim_token: Option<&str>,
        claim_lost: Option<&tokio::sync::watch::Receiver<bool>>,
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
                    claim_token,
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
            let step_exec = async {
                if let Some(ref tc) = parent_tc {
                    tc.scope_future(step_span, fut).await
                } else {
                    tracing::Instrument::instrument(fut, step_span).await
                }
            };
            let res = match run_cancellable_on_claim_loss(step_exec, claim_lost).await {
                Some(outcome) => outcome,
                None => {
                    return Err(SagaRecoveryError::new(
                        SagaError::Storage {
                            saga_id: Some(saga_id.to_string()),
                            message: format!(
                                "saga execution claim lost during step '{}'",
                                step.name()
                            ),
                        },
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
                            claim_token,
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
                            claim_token,
                            claim_lost,
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
                claim_token,
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
        claim_token: Option<&str>,
        claim_lost: Option<&tokio::sync::watch::Receiver<bool>>,
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
                    claim_token,
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
            let comp_exec = async {
                if let Some(ref tc) = parent_tc {
                    tc.scope_future(comp_span, fut).await
                } else {
                    tracing::Instrument::instrument(fut, comp_span).await
                }
            };
            let comp_res = match run_cancellable_on_claim_loss(comp_exec, claim_lost).await {
                Some(outcome) => outcome,
                None => {
                    return Err(SagaRecoveryError::new(
                        SagaError::Storage {
                            saga_id: Some(saga_id.to_string()),
                            message: format!(
                                "saga execution claim lost during compensation for step '{}'",
                                prev_step.name()
                            ),
                        },
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
                            claim_token,
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
                claim_token,
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
        claim_token: Option<&str>,
        claim_lost: Option<&tokio::sync::watch::Receiver<bool>>,
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
            claim_token,
            claim_lost,
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
        claim_token: Option<&str>,
        claim_lost: Option<&tokio::sync::watch::Receiver<bool>>,
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
            claim_token,
            claim_lost,
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
        let claim_token = claim.as_ref().map(|c| c.token.as_str());
        let claim_lost = claim.as_ref().map(|c| &c.claim_lost_rx);
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
                self.resume_compensation_recoverable(saga_id, claim_token, claim_lost, record, ctx)
                    .await
            }
        };
        if let Some(c) = claim {
            c.release().await;
        }
        res
    }
}

/// Polls `fut` to completion unless `claim_lost` transitions to `true`, in which case `fut`
/// is dropped immediately and `None` is returned (`RC-23`).
async fn run_cancellable_on_claim_loss<F, T>(
    fut: F,
    claim_lost: Option<&tokio::sync::watch::Receiver<bool>>,
) -> Option<T>
where
    F: std::future::Future<Output = T>,
{
    let Some(rx) = claim_lost else {
        return Some(fut.await);
    };
    let mut rx = rx.clone();
    if *rx.borrow() {
        return None;
    }
    tokio::pin!(fut);
    let lost = tokio::select! {
        biased;
        res = rx.wait_for(|&lost| lost) => res.is_ok(),
        outcome = &mut fut => return Some(outcome),
    };
    if lost { None } else { Some(fut.await) }
}

fn panic_message<'a>(payload: &'a Box<dyn std::any::Any + Send>, fallback: &'a str) -> &'a str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
        .unwrap_or(fallback)
}

#[cfg(test)]
mod claim_cancellation_tests {
    use super::*;
    use crate::saga::coordinator::BoxFuture;
    use std::time::Duration;

    #[derive(Debug)]
    struct LosingClaimStore {
        inner: InMemorySagaStore,
    }

    impl SagaStore for LosingClaimStore {
        fn save_record_cas(
            &self,
            record: &SagaRecord,
            expected_version: u64,
        ) -> BoxFuture<'_, Result<(), SagaError>> {
            self.inner.save_record_cas(record, expected_version)
        }
        fn save_record_cas_with_claim(
            &self,
            record: &SagaRecord,
            expected_version: u64,
            claim_token: Option<&str>,
        ) -> BoxFuture<'_, Result<(), SagaError>> {
            self.inner
                .save_record_cas_with_claim(record, expected_version, claim_token)
        }
        fn create_and_claim_record<'a>(
            &'a self,
            record: &'a SagaRecord,
        ) -> BoxFuture<'a, Result<String, SagaError>> {
            self.inner.create_and_claim_record(record)
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
        fn try_acquire_execution(&self, saga_id: &str) -> BoxFuture<'_, Result<String, SagaError>> {
            self.inner.try_acquire_execution(saga_id)
        }
        fn renew_execution(
            &self,
            _saga_id: &str,
            _claim_token: &str,
        ) -> BoxFuture<'_, Result<bool, SagaError>> {
            // Simulate immediate lease loss on the first renewal tick.
            Box::pin(async { Ok(false) })
        }
        fn claim_ttl(&self) -> Option<Duration> {
            Some(Duration::from_millis(60))
        }
        fn release_execution(
            &self,
            saga_id: &str,
            claim_token: &str,
        ) -> BoxFuture<'_, Result<(), SagaError>> {
            self.inner.release_execution(saga_id, claim_token)
        }
        fn release_execution_sync(&self, saga_id: &str, claim_token: &str) -> bool {
            self.inner.release_execution_sync(saga_id, claim_token)
        }
    }

    #[tokio::test]
    async fn saga_cancels_in_flight_step_immediately_on_claim_loss() {
        let store = Arc::new(LosingClaimStore {
            inner: InMemorySagaStore::new(),
        });
        let saga = Saga::<()>::new("cancel-on-lease-loss")
            .with_store(store)
            .step(
                "hang-forever",
                |_ctx| async {
                    std::future::pending::<()>().await;
                    Ok(())
                },
                |_ctx| async { Ok(()) },
            );

        let res = tokio::time::timeout(Duration::from_secs(2), saga.run(()))
            .await
            .expect("saga step must be cancelled promptly when execution claim is lost");
        let err = res
            .err()
            .expect("saga must fail when execution claim is lost");
        assert!(
            matches!(err, SagaError::Storage { ref message, .. } if message.contains("claim lost")),
            "expected Storage claim-lost error, got {err:?}"
        );
    }
}
