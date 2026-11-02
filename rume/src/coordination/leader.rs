//! Leader election interface and in-memory coordinator for active-passive failover.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::coordination::error::CoordinationError;
use crate::coordination::lock::CoordinationFuture;
use crate::helpers::safe_equal;

/// Interface for distributed leader election.
pub trait LeaderElector: Debug + Send + Sync + 'static {
    /// Attempts to acquire leadership for a role. Returns true if candidate became leader.
    fn try_acquire<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, bool>;

    /// Renews an actively held leadership lease.
    fn renew<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, bool>;

    /// Returns the current active leader for `role`, or `None` if no valid lease exists.
    fn get_leader<'a>(&'a self, role: &'a str) -> CoordinationFuture<'a, Option<String>>;

    /// Voluntarily resigns leadership.
    fn resign<'a>(&'a self, role: &'a str, candidate_id: &'a str) -> CoordinationFuture<'a, bool>;
}

#[derive(Debug, Clone)]
struct ActiveLeader {
    candidate_id: String,
    expires_at: Instant,
}

/// In-memory implementation of [`LeaderElector`] for development and multi-worker testing.
#[derive(Debug, Clone, Default)]
pub struct MemoryLeaderElector {
    leaders: Arc<RwLock<BTreeMap<String, ActiveLeader>>>,
}

impl MemoryLeaderElector {
    /// Creates a new in-memory leader elector.
    pub fn new() -> Self {
        Self {
            leaders: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }
}

impl LeaderElector for MemoryLeaderElector {
    fn try_acquire<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, bool> {
        Box::pin(async move {
            let mut leaders = self
                .leaders
                .write()
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let now = Instant::now();

            if let Some(current) = leaders.get(role)
                && current.expires_at > now
            {
                // Current leader's lease is still active
                if safe_equal(current.candidate_id.as_bytes(), candidate_id.as_bytes()) {
                    // Same candidate renewing via acquire
                    leaders.insert(
                        role.to_string(),
                        ActiveLeader {
                            candidate_id: candidate_id.to_string(),
                            expires_at: now + ttl,
                        },
                    );
                    return Ok(true);
                }
                return Ok(false);
            }

            // Unclaimed or expired: grant to candidate
            leaders.insert(
                role.to_string(),
                ActiveLeader {
                    candidate_id: candidate_id.to_string(),
                    expires_at: now + ttl,
                },
            );

            Ok(true)
        })
    }

    fn renew<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, bool> {
        Box::pin(async move {
            let mut leaders = self
                .leaders
                .write()
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let now = Instant::now();
            let Some(current) = leaders.get_mut(role) else {
                return Ok(false);
            };

            if current.expires_at > now
                && safe_equal(current.candidate_id.as_bytes(), candidate_id.as_bytes())
            {
                current.expires_at = now + ttl;
                return Ok(true);
            }

            Ok(false)
        })
    }

    fn get_leader<'a>(&'a self, role: &'a str) -> CoordinationFuture<'a, Option<String>> {
        Box::pin(async move {
            let leaders = self
                .leaders
                .read()
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let now = Instant::now();
            if let Some(current) = leaders.get(role)
                && current.expires_at > now
            {
                return Ok(Some(current.candidate_id.clone()));
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
