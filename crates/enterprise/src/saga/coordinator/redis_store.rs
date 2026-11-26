//! Redis-backed durable [`SagaStore`] using atomic Lua CAS and claim leases.

use std::fmt::Debug;
use std::time::Duration;

use deadpool_redis::Pool;

use super::BoxFuture;
use super::store::{SagaRecord, SagaStore};
use crate::saga::error::SagaError;

/// Durable Redis-backed [`SagaStore`].
#[derive(Clone)]
pub struct RedisSagaStore {
    pool: Pool,
    claim_ttl: Duration,
}

impl Debug for RedisSagaStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisSagaStore")
            .field("claim_ttl", &self.claim_ttl)
            .finish_non_exhaustive()
    }
}

impl RedisSagaStore {
    /// Creates a new [`RedisSagaStore`] with a 60-second default claim lease TTL.
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            claim_ttl: Duration::from_secs(60),
        }
    }

    /// Sets the execution claim lease TTL.
    pub fn with_claim_ttl(mut self, ttl: Duration) -> Self {
        self.claim_ttl = ttl;
        self
    }

    /// Returns a reference to the underlying Redis pool.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }
}

impl SagaStore for RedisSagaStore {
    fn save_record_cas(
        &self,
        _record: &SagaRecord,
        _expected_version: u64,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        todo!("Task 06: implement RedisSagaStore::save_record_cas")
    }

    fn load_record(&self, _saga_id: &str) -> BoxFuture<'_, Result<Option<SagaRecord>, SagaError>> {
        todo!("Task 06: implement RedisSagaStore::load_record")
    }

    fn list_incomplete(&self) -> BoxFuture<'_, Result<Vec<SagaRecord>, SagaError>> {
        todo!("Task 06: implement RedisSagaStore::list_incomplete")
    }

    fn try_acquire_execution(&self, _saga_id: &str) -> BoxFuture<'_, Result<String, SagaError>> {
        todo!("Task 06: implement RedisSagaStore::try_acquire_execution")
    }

    fn release_execution(
        &self,
        _saga_id: &str,
        _claim_token: &str,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        todo!("Task 06: implement RedisSagaStore::release_execution")
    }
}
