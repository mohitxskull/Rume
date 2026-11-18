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
    pub fn new(
        resource: impl Into<String>,
        lease_id: impl Into<String>,
        fencing_token: u64,
    ) -> Self {
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

    /// Renews an active lock lease for an additional `ttl`, updating `guard` with the
    /// refreshed expiration (and any updated fencing token).
    // Why: Taking `&'a mut LockGuard` instead of `&'a LockGuard` lets backends write back the
    // extended `expires_at` and any bumped `fencing_token` so callers never hold stale lease
    // metadata after renewal (RC-17). Revisit if a future backend requires interior-mutable
    // handles; pre-v1 breaking trait API change is accepted here.
    fn renew<'a>(&'a self, guard: &'a mut LockGuard, ttl: Duration) -> CoordinationFuture<'a, ()>;

    /// Releases an actively held lock. Returns true if released, false if already released or expired.
    fn release<'a>(&'a self, guard: LockGuard) -> CoordinationFuture<'a, bool>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug, Default)]
    struct RenewingFakeLock {
        next_token: AtomicU64,
    }

    impl DistributedLock for RenewingFakeLock {
        fn acquire<'a>(
            &'a self,
            resource: &'a str,
            ttl: Duration,
        ) -> CoordinationFuture<'a, Option<LockGuard>> {
            let resource = resource.to_string();
            Box::pin(async move {
                let token = self.next_token.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(Some(
                    LockGuard::new(resource, "lease-1", token)
                        .with_expires_at(std::time::Instant::now() + ttl),
                ))
            })
        }

        fn renew<'a>(
            &'a self,
            guard: &'a mut LockGuard,
            ttl: Duration,
        ) -> CoordinationFuture<'a, ()> {
            Box::pin(async move {
                guard.fencing_token = self.next_token.fetch_add(1, Ordering::SeqCst) + 1;
                guard.expires_at = Some(std::time::Instant::now() + ttl);
                Ok(())
            })
        }

        fn release<'a>(&'a self, _guard: LockGuard) -> CoordinationFuture<'a, bool> {
            Box::pin(async move { Ok(true) })
        }
    }

    #[tokio::test]
    async fn renew_updates_fencing_token_and_expires_at() {
        let lock = RenewingFakeLock::default();
        let initial_ttl = Duration::from_millis(50);
        let mut guard = lock
            .acquire("res", initial_ttl)
            .await
            .expect("acquire succeeds")
            .expect("guard acquired");

        let initial_token = guard.fencing_token;
        let initial_expires_at = guard.expires_at.expect("acquire sets expires_at");

        let renewed_ttl = Duration::from_secs(60);
        lock.renew(&mut guard, renewed_ttl)
            .await
            .expect("renew succeeds");

        assert!(
            guard.fencing_token > initial_token,
            "renew must update fencing_token on the guard (before={}, after={})",
            initial_token,
            guard.fencing_token
        );
        let renewed_expires_at = guard.expires_at.expect("renew sets expires_at");
        assert!(
            renewed_expires_at > initial_expires_at,
            "renew must extend expires_at on the guard"
        );
    }
}
