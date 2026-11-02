//! Production PostgreSQL-backed distributed locking and leader election.
//!
//! Provides:
//! - Atomic lock acquisition and TTL expiration backed by database rows
//! - Monotonically increasing 64-bit fencing tokens backed by PostgreSQL sequences (`rume_lock_fencing_seq`)
//! - Leader election with term fencing tokens across independent processes/nodes

use std::fmt::Debug;
use std::time::Duration;

use deadpool_postgres::Pool;
use uuid::Uuid;

use crate::coordination::error::CoordinationError;
use crate::coordination::leader::{LeaderElector, LeaderLease};
use crate::coordination::lock::{CoordinationFuture, DistributedLock, LockGuard};
use crate::persistence::postgres::PostgresBackend;

fn current_epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// PostgreSQL-backed implementation of [`DistributedLock`].
///
/// Guarantees:
/// - Mutual exclusion across separate OS processes and machines connected to the same database.
/// - Strictly monotonically increasing `fencing_token` issued via PostgreSQL sequence.
/// - Automatic TTL lease expiration without requiring manual dead worker cleanup.
#[derive(Clone)]
pub struct PostgresDistributedLock {
    pool: Pool,
}

impl Debug for PostgresDistributedLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresDistributedLock").finish()
    }
}

impl PostgresDistributedLock {
    /// Schema DDL for distributed lock storage and fencing token sequence.
    pub const SCHEMA: &'static str = r#"
CREATE TABLE IF NOT EXISTS rume_locks (
    resource TEXT PRIMARY KEY,
    lease_id TEXT NOT NULL,
    fencing_token BIGINT NOT NULL,
    expires_at BIGINT NOT NULL
);
CREATE SEQUENCE IF NOT EXISTS rume_lock_fencing_seq START 1;
"#;

    /// Creates a new distributed lock service from a `deadpool_postgres::Pool`.
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Creates a lock service from a [`PostgresBackend`].
    pub fn from_backend(backend: &PostgresBackend) -> Self {
        Self {
            pool: backend.pool().clone(),
        }
    }

    /// Initializes the `rume_locks` table and sequence if not already present.
    pub async fn init_schema(&self) -> Result<(), CoordinationError> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|e| CoordinationError::Backend(e.to_string()))?;
        client
            .batch_execute(Self::SCHEMA)
            .await
            .map_err(|e| CoordinationError::Backend(e.to_string()))?;
        Ok(())
    }
}

impl DistributedLock for PostgresDistributedLock {
    fn acquire<'a>(
        &'a self,
        resource: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LockGuard>> {
        let res = resource.to_string();
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let now = current_epoch_millis();
            let expires_at = now.saturating_add(ttl.as_millis() as i64);
            let lease_id = Uuid::new_v4().to_string();

            let stmt = r#"
INSERT INTO rume_locks (resource, lease_id, fencing_token, expires_at)
VALUES ($1, $2, nextval('rume_lock_fencing_seq'), $3)
ON CONFLICT (resource) DO UPDATE
SET lease_id = EXCLUDED.lease_id,
    fencing_token = nextval('rume_lock_fencing_seq'),
    expires_at = EXCLUDED.expires_at
WHERE rume_locks.expires_at <= $4
RETURNING fencing_token;
"#;
            let rows = client
                .query(stmt, &[&res, &lease_id, &expires_at, &now])
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(row) = rows.first() {
                let fencing_token: i64 = row.get(0);
                Ok(Some(LockGuard {
                    resource: res,
                    lease_id,
                    fencing_token: fencing_token as u64,
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn renew<'a>(&'a self, guard: &'a LockGuard, ttl: Duration) -> CoordinationFuture<'a, ()> {
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let now = current_epoch_millis();
            let new_expires_at = now.saturating_add(ttl.as_millis() as i64);

            let stmt = r#"
UPDATE rume_locks
SET expires_at = $1
WHERE resource = $2 AND lease_id = $3 AND expires_at > $4;
"#;
            let affected = client
                .execute(
                    stmt,
                    &[&new_expires_at, &guard.resource, &guard.lease_id, &now],
                )
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if affected == 0 {
                Err(CoordinationError::LockExpired {
                    resource: guard.resource.clone(),
                })
            } else {
                Ok(())
            }
        })
    }

    fn release<'a>(&'a self, guard: LockGuard) -> CoordinationFuture<'a, bool> {
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let stmt = "DELETE FROM rume_locks WHERE resource = $1 AND lease_id = $2";
            let affected = client
                .execute(stmt, &[&guard.resource, &guard.lease_id])
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            Ok(affected > 0)
        })
    }
}

/// PostgreSQL-backed implementation of [`LeaderElector`].
///
/// Guarantees:
/// - Single active leader per role across independent processes.
/// - Strictly monotonically increasing `term` issued via PostgreSQL sequence.
/// - Graceful resignation and automatic failover upon lease expiry.
#[derive(Clone)]
pub struct PostgresLeaderElector {
    pool: Pool,
}

impl Debug for PostgresLeaderElector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresLeaderElector").finish()
    }
}

impl PostgresLeaderElector {
    /// Schema DDL for leader election storage and term sequence.
    pub const SCHEMA: &'static str = r#"
CREATE TABLE IF NOT EXISTS rume_leaders (
    role TEXT PRIMARY KEY,
    candidate_id TEXT NOT NULL,
    term BIGINT NOT NULL,
    expires_at BIGINT NOT NULL
);
CREATE SEQUENCE IF NOT EXISTS rume_leader_term_seq START 1;
"#;

    /// Creates a new leader election service from a `deadpool_postgres::Pool`.
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Creates a leader election service from a [`PostgresBackend`].
    pub fn from_backend(backend: &PostgresBackend) -> Self {
        Self {
            pool: backend.pool().clone(),
        }
    }

    /// Initializes the `rume_leaders` table and sequence if not already present.
    pub async fn init_schema(&self) -> Result<(), CoordinationError> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|e| CoordinationError::Backend(e.to_string()))?;
        client
            .batch_execute(Self::SCHEMA)
            .await
            .map_err(|e| CoordinationError::Backend(e.to_string()))?;
        Ok(())
    }
}

impl LeaderElector for PostgresLeaderElector {
    fn try_acquire<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LeaderLease>> {
        let r = role.to_string();
        let c = candidate_id.to_string();
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let now = current_epoch_millis();
            let expires_at = now.saturating_add(ttl.as_millis() as i64);

            let stmt = r#"
INSERT INTO rume_leaders (role, candidate_id, term, expires_at)
VALUES ($1, $2, nextval('rume_leader_term_seq'), $3)
ON CONFLICT (role) DO UPDATE
SET candidate_id = EXCLUDED.candidate_id,
    term = nextval('rume_leader_term_seq'),
    expires_at = EXCLUDED.expires_at
WHERE rume_leaders.expires_at <= $4
RETURNING term;
"#;
            let rows = client
                .query(stmt, &[&r, &c, &expires_at, &now])
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(row) = rows.first() {
                let term: i64 = row.get(0);
                Ok(Some(LeaderLease {
                    role: r,
                    candidate_id: c,
                    term: term as u64,
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn renew<'a>(
        &'a self,
        role: &'a str,
        candidate_id: &'a str,
        ttl: Duration,
    ) -> CoordinationFuture<'a, Option<LeaderLease>> {
        let r = role.to_string();
        let c = candidate_id.to_string();
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let now = current_epoch_millis();
            let new_expires_at = now.saturating_add(ttl.as_millis() as i64);

            let stmt = r#"
UPDATE rume_leaders
SET expires_at = $1
WHERE role = $2 AND candidate_id = $3 AND expires_at > $4
RETURNING term;
"#;
            let rows = client
                .query(stmt, &[&new_expires_at, &r, &c, &now])
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(row) = rows.first() {
                let term: i64 = row.get(0);
                Ok(Some(LeaderLease {
                    role: r,
                    candidate_id: c,
                    term: term as u64,
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn get_leader<'a>(&'a self, role: &'a str) -> CoordinationFuture<'a, Option<LeaderLease>> {
        let r = role.to_string();
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let now = current_epoch_millis();
            let stmt =
                "SELECT candidate_id, term FROM rume_leaders WHERE role = $1 AND expires_at > $2";
            let rows = client
                .query(stmt, &[&r, &now])
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(row) = rows.first() {
                let candidate_id: String = row.get(0);
                let term: i64 = row.get(1);
                Ok(Some(LeaderLease {
                    role: r,
                    candidate_id,
                    term: term as u64,
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn resign<'a>(&'a self, role: &'a str, candidate_id: &'a str) -> CoordinationFuture<'a, bool> {
        let r = role.to_string();
        let c = candidate_id.to_string();
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            let stmt = "DELETE FROM rume_leaders WHERE role = $1 AND candidate_id = $2";
            let affected = client
                .execute(stmt, &[&r, &c])
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            Ok(affected > 0)
        })
    }
}
