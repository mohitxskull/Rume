//! Production PostgreSQL-backed distributed locking and leader election using SQLx.

use std::fmt::Debug;
use std::time::Duration;

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::coordination::error::CoordinationError;
use crate::coordination::leader::{LeaderElector, LeaderLease};
use crate::coordination::lock::{CoordinationFuture, DistributedLock, LockGuard};
use crate::persistence::postgres::PostgresBackend;

const LOCKS_SCHEMA: &str = include_str!("../persistence/sql/postgres/locks_schema.sql");
const LEADER_SCHEMA: &str = include_str!("../persistence/sql/postgres/leader_schema.sql");
const LOCK_ACQUIRE_SQL: &str = include_str!("../persistence/sql/postgres/lock_acquire.sql");
const LOCK_RENEW_SQL: &str = include_str!("../persistence/sql/postgres/lock_renew.sql");
const LEADER_ACQUIRE_SQL: &str = include_str!("../persistence/sql/postgres/leader_acquire.sql");
const LEADER_RENEW_SQL: &str = include_str!("../persistence/sql/postgres/leader_renew.sql");

/// PostgreSQL-backed implementation of [`DistributedLock`].
#[derive(Clone)]
pub struct PostgresDistributedLock {
    pool: PgPool,
}

impl Debug for PostgresDistributedLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresDistributedLock").finish()
    }
}

impl PostgresDistributedLock {
    /// Schema DDL for distributed lock storage and fencing token sequence.
    pub const SCHEMA: &'static str = LOCKS_SCHEMA;

    /// Creates a new distributed lock service from a `PgPool`.
    pub fn new(pool: PgPool) -> Self {
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
        sqlx::raw_sql(Self::SCHEMA)
            .execute(&self.pool)
            .await
            .map_err(|e| CoordinationError::Backend(e.to_string()))?;
        Ok(())
    }

    /// Prunes expired locks from the database to prevent unbounded table bloat.
    pub async fn prune_expired(&self) -> Result<usize, CoordinationError> {
        let res = sqlx::query("DELETE FROM rume_locks WHERE expires_at <= (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT")
            .execute(&self.pool)
            .await
            .map_err(|e| CoordinationError::Backend(e.to_string()))?;
        Ok(res.rows_affected() as usize)
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
            let ttl_ms = ttl.as_millis().max(1) as i64;
            let lease_id = Uuid::new_v4().to_string();

            let row = sqlx::query(LOCK_ACQUIRE_SQL)
                .bind(&res)
                .bind(&lease_id)
                .bind(ttl_ms)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(r) = row {
                let fencing_token: i64 = r.try_get(0).map_err(|e| CoordinationError::Backend(e.to_string()))?;
                Ok(Some(LockGuard {
                    resource: res,
                    lease_id,
                    fencing_token: fencing_token as u64,
                    expires_at: Some(std::time::Instant::now() + ttl),
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn renew<'a>(&'a self, guard: &'a LockGuard, ttl: Duration) -> CoordinationFuture<'a, ()> {
        Box::pin(async move {
            let ttl_ms = ttl.as_millis().max(1) as i64;

            let res = sqlx::query(LOCK_RENEW_SQL)
                .bind(ttl_ms)
                .bind(&guard.resource)
                .bind(&guard.lease_id)
                .execute(&self.pool)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if res.rows_affected() == 0 {
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
            let stmt = "DELETE FROM rume_locks WHERE resource = $1 AND lease_id = $2";
            let res = sqlx::query(stmt)
                .bind(&guard.resource)
                .bind(&guard.lease_id)
                .execute(&self.pool)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            Ok(res.rows_affected() > 0)
        })
    }
}

/// PostgreSQL-backed implementation of [`LeaderElector`].
#[derive(Clone)]
pub struct PostgresLeaderElector {
    pool: PgPool,
}

impl Debug for PostgresLeaderElector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresLeaderElector").finish()
    }
}

impl PostgresLeaderElector {
    /// Schema DDL for leader election storage and term sequence.
    pub const SCHEMA: &'static str = LEADER_SCHEMA;

    /// Creates a new leader election service from a `PgPool`.
    pub fn new(pool: PgPool) -> Self {
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
        sqlx::raw_sql(Self::SCHEMA)
            .execute(&self.pool)
            .await
            .map_err(|e| CoordinationError::Backend(e.to_string()))?;
        Ok(())
    }

    /// Prunes expired leader leases from the database.
    pub async fn prune_expired(&self) -> Result<usize, CoordinationError> {
        let res = sqlx::query("DELETE FROM rume_leaders WHERE expires_at <= (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT")
            .execute(&self.pool)
            .await
            .map_err(|e| CoordinationError::Backend(e.to_string()))?;
        Ok(res.rows_affected() as usize)
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
        let cid = candidate_id.to_string();
        Box::pin(async move {
            let ttl_ms = ttl.as_millis().max(1) as i64;

            let row = sqlx::query(LEADER_ACQUIRE_SQL)
                .bind(&r)
                .bind(&cid)
                .bind(ttl_ms)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(row_data) = row {
                let term: i64 = row_data.try_get(0).map_err(|e| CoordinationError::Backend(e.to_string()))?;
                Ok(Some(LeaderLease {
                    role: r,
                    candidate_id: cid,
                    term: term as u64,
                    expires_at: Some(std::time::Instant::now() + ttl),
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
        let cid = candidate_id.to_string();
        Box::pin(async move {
            let ttl_ms = ttl.as_millis().max(1) as i64;

            let row = sqlx::query(LEADER_RENEW_SQL)
                .bind(ttl_ms)
                .bind(&r)
                .bind(&cid)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(row_data) = row {
                let term: i64 = row_data.try_get(0).map_err(|e| CoordinationError::Backend(e.to_string()))?;
                Ok(Some(LeaderLease {
                    role: r,
                    candidate_id: cid,
                    term: term as u64,
                    expires_at: Some(std::time::Instant::now() + ttl),
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn get_leader<'a>(&'a self, role: &'a str) -> CoordinationFuture<'a, Option<LeaderLease>> {
        let r = role.to_string();
        Box::pin(async move {
            let stmt = "SELECT role, candidate_id, term FROM rume_leaders WHERE role = $1 AND expires_at > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT";
            let row = sqlx::query(stmt)
                .bind(&r)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            if let Some(row_data) = row {
                let role: String = row_data.try_get(0).map_err(|e| CoordinationError::Backend(e.to_string()))?;
                let candidate_id: String = row_data.try_get(1).map_err(|e| CoordinationError::Backend(e.to_string()))?;
                let term: i64 = row_data.try_get(2).map_err(|e| CoordinationError::Backend(e.to_string()))?;
                Ok(Some(LeaderLease {
                    role,
                    candidate_id,
                    term: term as u64,
                    expires_at: None,
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn resign<'a>(&'a self, role: &'a str, candidate_id: &'a str) -> CoordinationFuture<'a, bool> {
        let r = role.to_string();
        let cid = candidate_id.to_string();
        Box::pin(async move {
            let stmt = "DELETE FROM rume_leaders WHERE role = $1 AND candidate_id = $2";
            let res = sqlx::query(stmt)
                .bind(&r)
                .bind(&cid)
                .execute(&self.pool)
                .await
                .map_err(|e| CoordinationError::Backend(e.to_string()))?;

            Ok(res.rows_affected() > 0)
        })
    }
}
