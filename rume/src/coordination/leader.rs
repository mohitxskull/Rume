//! Leader election interface and in-memory coordinator for active-passive failover.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::coordination::error::CoordinationError;
use crate::coordination::lock::CoordinationFuture;
use crate::helpers::safe_equal;

/// A lease granted to an elected leader, containing a monotonic term to prevent split-brain mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderLease {
    /// Target leadership role.
    pub role: String,
    /// Candidate identifier holding the lease.
    pub candidate_id: String,
    /// Monotonically increasing election term / fencing token.
    pub term: u64,
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

#[derive(Debug, Clone)]
struct ActiveLeader {
    candidate_id: String,
    term: u64,
    expires_at: Instant,
}

/// In-memory implementation of [`LeaderElector`] for development and multi-worker testing.
#[derive(Debug, Clone, Default)]
pub struct MemoryLeaderElector {
    leaders: Arc<RwLock<BTreeMap<String, ActiveLeader>>>,
    term_counter: Arc<RwLock<u64>>,
}

impl MemoryLeaderElector {
    /// Creates a new in-memory leader elector.
    pub fn new() -> Self {
        Self {
            leaders: Arc::new(RwLock::new(BTreeMap::new())),
            term_counter: Arc::new(RwLock::new(0)),
        }
    }
}

impl LeaderElector for MemoryLeaderElector {
    fn try_acquire<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LeaderLease>> {
        Box::pin(async move {
            let mut leaders = self
                .leaders
                .write()
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let now = Instant::now();

            if let Some(current) = leaders.get_mut(role)
                && current.expires_at > now
            {
                // Current leader's lease is still active
                if safe_equal(current.candidate_id.as_bytes(), candidate_id.as_bytes()) {
                    // Same candidate renewing via acquire
                    current.expires_at = now + ttl;
                    return Ok(Some(LeaderLease {
                        role: role.to_string(),
                        candidate_id: candidate_id.to_string(),
                        term: current.term,
                    }));
                }
                return Ok(None);
            }

            // Unclaimed or expired: allocate next monotonic term and grant to candidate
            let mut term_guard = self
                .term_counter
                .write()
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;
            *term_guard += 1;
            let term = *term_guard;

            leaders.insert(
                role.to_string(),
                ActiveLeader {
                    candidate_id: candidate_id.to_string(),
                    term,
                    expires_at: now + ttl,
                },
            );

            Ok(Some(LeaderLease {
                role: role.to_string(),
                candidate_id: candidate_id.to_string(),
                term,
            }))
        })
    }

    fn renew<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LeaderLease>> {
        Box::pin(async move {
            let mut leaders = self
                .leaders
                .write()
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let now = Instant::now();
            let Some(current) = leaders.get_mut(role) else {
                return Ok(None);
            };

            if current.expires_at > now
                && safe_equal(current.candidate_id.as_bytes(), candidate_id.as_bytes())
            {
                current.expires_at = now + ttl;
                return Ok(Some(LeaderLease {
                    role: role.to_string(),
                    candidate_id: candidate_id.to_string(),
                    term: current.term,
                }));
            }

            Ok(None)
        })
    }

    fn get_leader<'a>(&'a self, role: &'a str) -> CoordinationFuture<'a, Option<LeaderLease>> {
        Box::pin(async move {
            let leaders = self
                .leaders
                .read()
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let now = Instant::now();
            if let Some(current) = leaders.get(role)
                && current.expires_at > now
            {
                return Ok(Some(LeaderLease {
                    role: role.to_string(),
                    candidate_id: current.candidate_id.clone(),
                    term: current.term,
                }));
            }

            Ok(None)
        })
    }

    fn resign<'a>(&'a self, role: &'a str, candidate_id: &'a str) -> CoordinationFuture<'a, bool> {
        Box::pin(async move {
            let mut leaders = self
                .leaders
                .write()
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(current) = leaders.get(role)
                && safe_equal(current.candidate_id.as_bytes(), candidate_id.as_bytes())
            {
                leaders.remove(role);
                return Ok(true);
            }

            Ok(false)
        })
    }
}
