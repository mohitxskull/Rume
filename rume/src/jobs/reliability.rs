//! Job reliability: idempotency keys, worker heartbeats, and abandoned job recovery.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::jobs::error::JobError;
use crate::jobs::job::BoxFuture;

/// Status returned when attempting to acquire an idempotency lock for a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyStatus {
    /// Key was not previously acquired; execution may proceed.
    New,
    /// Execution for this key is currently in progress by another worker.
    InProgress,
    /// Execution for this key previously succeeded; contains optional cached result bytes.
    Completed(Option<Vec<u8>>),
}

/// Storage interface for ensuring exactly-once or at-most-once job processing via idempotency keys.
pub trait IdempotencyStore: Debug + Send + Sync + 'static {
    /// Attempts to acquire an idempotency lock for `key` valid for `ttl`.
    fn try_acquire(
        &self,
        key: &str,
        ttl: Duration,
    ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>>;

    /// Marks the execution as completed, saving an optional result payload.
    fn complete(&self, key: &str, result: Option<Vec<u8>>) -> BoxFuture<'_, Result<(), JobError>>;

    /// Releases an in-progress lock (e.g. after failure), permitting re-execution.
    fn release(&self, key: &str) -> BoxFuture<'_, Result<(), JobError>>;
}

#[derive(Debug, Clone)]
enum IdempotencyEntry {
    InProgress(Instant),
    Completed {
        expires_at: Instant,
        result: Option<Vec<u8>>,
    },
}

#[derive(Debug, Default)]
struct MemoryIdempotencyState {
    entries: BTreeMap<String, IdempotencyEntry>,
}

/// In-memory idempotency store with TTL expiration.
#[derive(Debug, Clone, Default)]
pub struct MemoryIdempotencyStore {
    state: Arc<RwLock<MemoryIdempotencyState>>,
}

impl MemoryIdempotencyStore {
    /// Creates a new in-memory idempotency store.
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(MemoryIdempotencyState::default())),
        }
    }
}

impl IdempotencyStore for MemoryIdempotencyStore {
    fn try_acquire(
        &self,
        key: &str,
        ttl: Duration,
    ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>> {
        let k = key.to_string();
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let now = Instant::now();

            if let Some(entry) = state.entries.get(&k) {
                match entry {
                    IdempotencyEntry::InProgress(expires_at) => {
                        if *expires_at > now {
                            return Ok(IdempotencyStatus::InProgress);
                        }
                    }
                    IdempotencyEntry::Completed { expires_at, result } => {
                        if *expires_at > now {
                            return Ok(IdempotencyStatus::Completed(result.clone()));
                        }
                    }
                }
            }

            // Acquire new
            state
                .entries
                .insert(k, IdempotencyEntry::InProgress(now + ttl));
            Ok(IdempotencyStatus::New)
        })
    }

    fn complete(&self, key: &str, result: Option<Vec<u8>>) -> BoxFuture<'_, Result<(), JobError>> {
        let k = key.to_string();
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            // Retain completed state for 24 hours default
            let expires_at = Instant::now() + Duration::from_secs(86400);
            state
                .entries
                .insert(k, IdempotencyEntry::Completed { expires_at, result });
            Ok(())
        })
    }

    fn release(&self, key: &str) -> BoxFuture<'_, Result<(), JobError>> {
        let k = key.to_string();
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            state.entries.remove(&k);
            Ok(())
        })
    }
}

/// Registry tracking distributed worker heartbeats and detecting dead nodes.
pub trait HeartbeatRegistry: Debug + Send + Sync + 'static {
    /// Records or updates a heartbeat for a worker, valid for `ttl`.
    fn record_heartbeat(
        &self,
        worker_id: &str,
        ttl: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>>;

    /// Checks if a worker's heartbeat is currently active.
    fn is_alive(&self, worker_id: &str) -> BoxFuture<'_, Result<bool, JobError>>;

    /// Removes and returns identifiers of workers whose heartbeat has expired.
    fn reap_dead_workers(&self) -> BoxFuture<'_, Result<Vec<String>, JobError>>;
}

#[derive(Debug, Default)]
struct MemoryHeartbeatState {
    workers: BTreeMap<String, Instant>,
}

/// In-memory heartbeat tracker for distributed worker health.
#[derive(Debug, Clone, Default)]
pub struct MemoryHeartbeatRegistry {
    state: Arc<RwLock<MemoryHeartbeatState>>,
}

impl MemoryHeartbeatRegistry {
    /// Creates a new heartbeat registry.
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(MemoryHeartbeatState::default())),
        }
    }
}

impl HeartbeatRegistry for MemoryHeartbeatRegistry {
    fn record_heartbeat(
        &self,
        worker_id: &str,
        ttl: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        let wid = worker_id.to_string();
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            state.workers.insert(wid, Instant::now() + ttl);
            Ok(())
        })
    }

    fn is_alive(&self, worker_id: &str) -> BoxFuture<'_, Result<bool, JobError>> {
        let wid = worker_id.to_string();
        Box::pin(async move {
            let state = self
                .state
                .read()
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            if let Some(exp) = state.workers.get(&wid) {
                Ok(Instant::now() < *exp)
            } else {
                Ok(false)
            }
        })
    }

    fn reap_dead_workers(&self) -> BoxFuture<'_, Result<Vec<String>, JobError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            let now = Instant::now();
            let mut dead = Vec::new();

            state.workers.retain(|wid, expires_at| {
                if *expires_at <= now {
                    dead.push(wid.clone());
                    false
                } else {
                    true
                }
            });

            Ok(dead)
        })
    }
}
