//! Distributed lock abstractions with lease fences and TTL auto-expiration.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::coordination::error::CoordinationError;

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
    /// Expiration timestamp of the lease.
    pub expires_at: Option<std::time::Instant>,
}

impl LockGuard {
    /// Creates a new LockGuard.
    pub fn new(resource: impl Into<String>, lease_id: impl Into<String>, fencing_token: u64) -> Self {
        Self {
            resource: resource.into(),
            lease_id: lease_id.into(),
            fencing_token,
            expires_at: None,
        }
    }

    /// Attaches an expiration timestamp.
    pub fn with_expires_at(mut self, expires_at: std::time::Instant) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    /// Checks if this lock guard's lease has expired according to monotonic clock.
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .map(|exp| std::time::Instant::now() >= exp)
            .unwrap_or(false)
    }

    /// Returns the remaining duration before expiration, or None if already expired or unbounded.
    pub fn time_remaining(&self) -> Option<Duration> {
        self.expires_at
            .and_then(|exp| exp.checked_duration_since(std::time::Instant::now()))
    }
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
