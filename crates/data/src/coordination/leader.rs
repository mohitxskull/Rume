//! Leader election interface and lease types for active-passive failover.

use std::fmt::Debug;
use std::time::Duration;

use crate::coordination::lock::CoordinationFuture;

/// A lease granted to an elected leader, containing a monotonic term to prevent split-brain mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderLease {
    /// Target leadership role.
    pub role: String,
    /// Candidate identifier holding the lease.
    pub candidate_id: String,
    /// Monotonically increasing election term / fencing token.
    pub term: u64,
    /// Expiration timestamp of the lease.
    pub expires_at: Option<std::time::Instant>,
}

impl LeaderLease {
    /// Creates a new LeaderLease.
    pub fn new(role: impl Into<String>, candidate_id: impl Into<String>, term: u64) -> Self {
        Self {
            role: role.into(),
            candidate_id: candidate_id.into(),
            term,
            expires_at: None,
        }
    }

    /// Attaches an expiration timestamp.
    pub fn with_expires_at(mut self, expires_at: std::time::Instant) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    /// Checks if this leadership lease has expired according to monotonic clock.
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

/// Interface for distributed leader election.
pub trait LeaderElector: Debug + Send + Sync + 'static {
    /// Attempts to acquire leadership for a role. Returns `Some(LeaderLease)` if acquired.
    fn try_acquire<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LeaderLease>>;

    /// Renews an actively held leadership lease. Returns `Some(LeaderLease)` on success.
    fn renew<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LeaderLease>>;

    /// Returns the current active leader for `role`, or `None` if no valid lease exists.
    fn get_leader<'a>(&'a self, role: &'a str) -> CoordinationFuture<'a, Option<LeaderLease>>;

    /// Voluntarily resigns leadership.
    fn resign<'a>(&'a self, role: &'a str, candidate_id: &'a str) -> CoordinationFuture<'a, bool>;
}
