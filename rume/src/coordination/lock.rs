//! Distributed lock abstractions with lease fences and TTL auto-expiration.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::coordination::error::CoordinationError;
use crate::helpers::safe_equal;

/// Future type alias for coordination operations.
pub type CoordinationFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, CoordinationError>> + Send + 'a>>;

/// An acquired lock token with monotonic fencing counter to prevent split-brain mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockGuard {
    /// Target resource name.
    pub resource: String,
    /// Unique lease identifier.
    pub lease_id: String,
    /// Monotonically increasing fencing token.
    pub fencing_token: u64,
}

/// Interface for distributed locking.
pub trait DistributedLock: Debug + Send + Sync + 'static {
    /// Attempts to acquire an exclusive lock on `resource` valid for `ttl`.
    ///
    /// Returns `Some(LockGuard)` if acquired, or `None` if already locked.
    fn acquire<'a>(
        &'a self,
        resource: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LockGuard>>;

    /// Renews an active lock lease for an additional `ttl`.
    fn renew<'a>(&'a self, guard: &'a LockGuard, ttl: Duration) -> CoordinationFuture<'a, ()>;

    /// Releases an actively held lock. Returns true if released, false if already released or expired.
    fn release<'a>(&'a self, guard: LockGuard) -> CoordinationFuture<'a, bool>;
}

#[derive(Debug, Clone)]
struct ActiveLock {
    lease_id: String,
    expires_at: Instant,
}

/// In-memory implementation of [`DistributedLock`] for development and integration tests.
#[derive(Debug, Clone, Default)]
pub struct MemoryDistributedLock {
    locks: Arc<RwLock<BTreeMap<String, ActiveLock>>>,
    counter: Arc<RwLock<u64>>,
}

impl MemoryDistributedLock {
    /// Creates a new in-memory lock coordinator.
    pub fn new() -> Self {
        Self {
            locks: Arc::new(RwLock::new(BTreeMap::new())),
            counter: Arc::new(RwLock::new(0)),
        }
    }
}

impl DistributedLock for MemoryDistributedLock {
    fn acquire<'a>(
        &'a self,
        resource: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LockGuard>> {
        Box::pin(async move {
            let mut locks = self
                .locks
                .write()
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let now = Instant::now();

            if let Some(existing) = locks.get(resource)
                && existing.expires_at > now
            {
                return Ok(None);
            }

            let mut counter = self
                .counter
                .write()
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;
            *counter += 1;
            let fencing_token = *counter;

            let lease_id = Uuid::new_v4().to_string();
            locks.insert(
                resource.to_string(),
                ActiveLock {
                    lease_id: lease_id.clone(),
                    expires_at: now + ttl,
                },
            );

            Ok(Some(LockGuard {
                resource: resource.to_string(),
                lease_id,
                fencing_token,
            }))
        })
    }

    fn renew<'a>(&'a self, guard: &'a LockGuard, ttl: Duration) -> CoordinationFuture<'a, ()> {
        Box::pin(async move {
            let mut locks = self
                .locks
                .write()
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let now = Instant::now();
            let Some(active) = locks.get_mut(&guard.resource) else {
                return Err(CoordinationError::LockExpired {
                    resource: guard.resource.clone(),
                });
            };

            if !safe_equal(active.lease_id.as_bytes(), guard.lease_id.as_bytes()) {
                return Err(CoordinationError::LockUnavailable {
                    resource: guard.resource.clone(),
                    reason: "lease ID mismatch".into(),
                });
            }

            if active.expires_at <= now {
                return Err(CoordinationError::LockExpired {
                    resource: guard.resource.clone(),
                });
            }

            active.expires_at = now + ttl;
            Ok(())
        })
    }

    fn release<'a>(&'a self, guard: LockGuard) -> CoordinationFuture<'a, bool> {
        Box::pin(async move {
            let mut locks = self
                .locks
                .write()
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(active) = locks.get(&guard.resource)
                && safe_equal(active.lease_id.as_bytes(), guard.lease_id.as_bytes())
            {
                locks.remove(&guard.resource);
                return Ok(true);
            }

            Ok(false)
        })
    }
}
