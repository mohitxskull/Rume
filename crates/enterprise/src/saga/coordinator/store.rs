//! Saga persistence store: the status types, the `SagaStore` trait and `InMemorySagaStore`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use super::BoxFuture;
use crate::saga::error::SagaError;

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

    /// Atomically saves or updates a saga execution record with both version CAS
    /// and optional execution claim fencing (`claim_token`).
    ///
    /// When `claim_token` is `Some` (and `expected_version > 0`), stores that enforce
    /// claim fencing reject the write if another worker currently owns the execution claim.
    fn save_record_cas_with_claim(
        &self,
        record: &SagaRecord,
        expected_version: u64,
        claim_token: Option<&str>,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        let _ = claim_token;
        self.save_record_cas(record, expected_version)
    }

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
    fn try_acquire_execution(&self, saga_id: &str) -> BoxFuture<'_, Result<String, SagaError>>;

    /// Renews the lease for an active execution claim held under `claim_token`.
    ///
    /// Returns `Ok(true)` if the lease was extended, `Ok(false)` if the claim was
    /// explicitly lost or expired, or `Err(SagaError::Storage)` on a transient
    /// storage/transport failure.
    fn renew_execution(
        &self,
        saga_id: &str,
        claim_token: &str,
    ) -> BoxFuture<'_, Result<bool, SagaError>> {
        let _ = (saga_id, claim_token);
        Box::pin(async { Ok(true) })
    }

    /// Returns the claim lease TTL if this store uses time-bounded execution leases
    /// requiring background heartbeat renewal while steps execute.
    fn claim_ttl(&self) -> Option<std::time::Duration> {
        None
    }

    /// Releases a previously acquired execution claim for `saga_id`.
    fn release_execution(
        &self,
        saga_id: &str,
        claim_token: &str,
    ) -> BoxFuture<'_, Result<(), SagaError>>;

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
    pub(super) records: Arc<RwLock<HashMap<String, SagaRecord>>>,
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
        self.save_record_cas_with_claim(record, expected_version, None)
    }

    fn save_record_cas_with_claim(
        &self,
        record: &SagaRecord,
        expected_version: u64,
        claim_token: Option<&str>,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        let record = record.clone();
        let claim_token = claim_token.map(str::to_string);
        let records = Arc::clone(&self.records);
        let active_claims = Arc::clone(&self.active_claims);
        let max_retained = self.max_retained_terminal;
        Box::pin(async move {
            let mut map = records.write().await;
            if expected_version > 0
                && let Some(token) = claim_token.as_deref()
            {
                let claims = active_claims.lock().map_err(|_| SagaError::Storage {
                    saga_id: Some(record.saga_id.clone()),
                    message: "saga claim lock poisoned".into(),
                })?;
                if claims.get(&record.saga_id).map(String::as_str) != Some(token) {
                    return Err(SagaError::Storage {
                        saga_id: Some(record.saga_id.clone()),
                        message: "saga execution claim mismatch".into(),
                    });
                }
            }
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
/// compensation callbacks, renewing its lease in the background and releasing it on
/// normal completion or task cancellation.
pub(super) struct ExecutionClaimGuard {
    pub(super) store: Option<Arc<dyn SagaStore>>,
    pub(super) saga_id: String,
    pub(super) token: String,
    pub(super) renew_task: Option<tokio::task::JoinHandle<()>>,
}

impl ExecutionClaimGuard {
    pub(super) async fn release(mut self) {
        if let Some(handle) = self.renew_task.take() {
            handle.abort();
        }
        // Why: keep `self.store` populated until `release_execution` returns `Ok(())` so if
        // this future is cancelled mid-await or the backend returns a transient error, `Drop`
        // still holds the store and runs fallback cleanup instead of stranding the claim.
        if let Some(store) = self.store.as_ref()
            && store
                .release_execution(&self.saga_id, &self.token)
                .await
                .is_ok()
        {
            self.store = None;
        }
    }
}

impl Drop for ExecutionClaimGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.renew_task.take() {
            handle.abort();
        }
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
            } else {
                // Why: async-only `SagaStore` implementations dropped outside a Tokio
                // runtime have no `Handle::try_current()`; run a bounded release on a
                // detached helper thread with its own current-thread runtime so the claim
                // is not permanently leaked.
                let _ = std::thread::spawn(move || {
                    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        rt.block_on(async move {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(5),
                                store.release_execution(&saga_id, &token),
                            )
                            .await;
                        });
                    }
                });
            }
        }
    }
}
