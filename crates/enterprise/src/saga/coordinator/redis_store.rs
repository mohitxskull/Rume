//! Redis-backed durable [`SagaStore`] using atomic Lua CAS and claim leases.

use std::fmt::Debug;
use std::time::Duration;

use deadpool_redis::Pool;
use deadpool_redis::redis::Script;

use super::BoxFuture;
use super::store::{SagaRecord, SagaStore};
use crate::saga::error::SagaError;

// Why: Share the `{rume:saga}` Redis Cluster hash tag across record keys, claim keys,
// and the incomplete index set so multi-key Lua scripts (`CAS_SAVE_LUA`) and `MGET`
// in `list_incomplete` always map to the same hash slot and never trigger `CROSSSLOT`.
const INCOMPLETE_SET_KEY: &str = "{rume:saga}:incomplete";

const CAS_SAVE_LUA: &str = r#"
local raw = redis.call('GET', KEYS[1])
local current_version = 0
if raw then
    local ok, decoded = pcall(cjson.decode, raw)
    if not ok or type(decoded) ~= 'table' then
        return {-1, 0}
    end
    current_version = tonumber(decoded.version) or 0
end
if current_version ~= tonumber(ARGV[1]) then
    return {0, current_version}
end
if ARGV[5] ~= '' and tonumber(ARGV[1]) > 0 then
    if redis.call('GET', KEYS[3]) ~= ARGV[5] then
        return {-2, current_version}
    end
end
if ARGV[4] == '1' then
    local ttl_ms = tonumber(ARGV[6]) or 0
    if ttl_ms > 0 then
        redis.call('SET', KEYS[1], ARGV[2], 'PX', ttl_ms)
    else
        redis.call('SET', KEYS[1], ARGV[2])
    end
    redis.call('SREM', KEYS[2], ARGV[3])
else
    redis.call('SET', KEYS[1], ARGV[2])
    redis.call('SADD', KEYS[2], ARGV[3])
end
return {1, current_version}
"#;

const RENEW_CLAIM_LUA: &str = r#"
if redis.call('GET', KEYS[1]) == ARGV[1] then
    return redis.call('PEXPIRE', KEYS[1], tonumber(ARGV[2]))
end
return 0
"#;

const RELEASE_CLAIM_LUA: &str = r#"
if redis.call('GET', KEYS[1]) == ARGV[1] then
    return redis.call('DEL', KEYS[1])
end
return 0
"#;

/// Durable Redis-backed [`SagaStore`].
#[derive(Clone)]
pub struct RedisSagaStore {
    pool: Pool,
    claim_ttl: Duration,
    terminal_ttl: Option<Duration>,
}

impl Debug for RedisSagaStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisSagaStore")
            .field("claim_ttl", &self.claim_ttl)
            .field("terminal_ttl", &self.terminal_ttl)
            .finish_non_exhaustive()
    }
}

impl RedisSagaStore {
    /// Creates a new [`RedisSagaStore`] with a 60-second default claim lease TTL.
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            claim_ttl: Duration::from_secs(60),
            terminal_ttl: None,
        }
    }

    /// Sets the execution claim lease TTL.
    pub fn with_claim_ttl(mut self, ttl: Duration) -> Self {
        self.claim_ttl = ttl;
        self
    }

    /// Configures an optional TTL applied to terminal (`Success`, `Compensated`,
    /// `CompensationFailed`) saga records so completed history expires automatically.
    pub fn with_terminal_ttl(mut self, ttl: Duration) -> Self {
        self.terminal_ttl = Some(ttl);
        self
    }

    /// Returns a reference to the underlying Redis pool.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    fn record_key(saga_id: &str) -> String {
        format!("{{rume:saga}}:record:{saga_id}")
    }

    fn claim_key(saga_id: &str) -> String {
        format!("{{rume:saga}}:claim:{saga_id}")
    }
}

fn storage_err(saga_id: Option<&str>, err: impl std::fmt::Display) -> SagaError {
    SagaError::Storage {
        saga_id: saga_id.map(str::to_string),
        message: err.to_string(),
    }
}

impl SagaStore for RedisSagaStore {
    fn save_record_cas(
        &self,
        record: &SagaRecord,
        expected_version: u64,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        self.save_record_cas_with_claim(record, expected_version, None)
    }

    fn save_record_cas_with_claim(
        &self,
        record: &SagaRecord,
        expected_version: u64,
        claim_token: Option<&str>,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        let record = record.clone();
        let claim_token = claim_token.unwrap_or("").to_string();
        let terminal_ttl_ms = self
            .terminal_ttl
            .map(|d| u64::try_from(d.as_millis().max(1)).unwrap_or(u64::MAX))
            .unwrap_or(0);
        Box::pin(async move {
            let payload = serde_json::to_string(&record)
                .map_err(|e| storage_err(Some(&record.saga_id), e))?;
            let is_terminal = if record.status.is_terminal() {
                "1"
            } else {
                "0"
            };

            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| storage_err(Some(&record.saga_id), e))?;

            let (ok, current_version): (i32, u64) = Script::new(CAS_SAVE_LUA)
                .key(Self::record_key(&record.saga_id))
                .key(INCOMPLETE_SET_KEY)
                .key(Self::claim_key(&record.saga_id))
                .arg(expected_version)
                .arg(payload)
                .arg(&record.saga_id)
                .arg(is_terminal)
                .arg(&claim_token)
                .arg(terminal_ttl_ms)
                .invoke_async(&mut conn)
                .await
                .map_err(|e| storage_err(Some(&record.saga_id), e))?;

            match ok {
                1 => Ok(()),
                -1 => Err(SagaError::Storage {
                    saga_id: Some(record.saga_id),
                    message: "corrupt saga record JSON in Redis".into(),
                }),
                -2 => Err(SagaError::Storage {
                    saga_id: Some(record.saga_id),
                    message: "saga execution claim mismatch".into(),
                }),
                _ => Err(SagaError::Storage {
                    saga_id: Some(record.saga_id),
                    message: format!(
                        "saga version conflict: expected version {expected_version}, found {current_version}"
                    ),
                }),
            }
        })
    }

    fn load_record(&self, saga_id: &str) -> BoxFuture<'_, Result<Option<SagaRecord>, SagaError>> {
        let saga_id = saga_id.to_string();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| storage_err(Some(&saga_id), e))?;

            let raw: Option<String> = deadpool_redis::redis::cmd("GET")
                .arg(Self::record_key(&saga_id))
                .query_async(&mut conn)
                .await
                .map_err(|e| storage_err(Some(&saga_id), e))?;

            raw.map(|json| serde_json::from_str::<SagaRecord>(&json))
                .transpose()
                .map_err(|e| storage_err(Some(&saga_id), e))
        })
    }

    fn list_incomplete(&self) -> BoxFuture<'_, Result<Vec<SagaRecord>, SagaError>> {
        Box::pin(async move {
            let mut conn = self.pool.get().await.map_err(|e| storage_err(None, e))?;

            let ids: Vec<String> = deadpool_redis::redis::cmd("SMEMBERS")
                .arg(INCOMPLETE_SET_KEY)
                .query_async(&mut conn)
                .await
                .map_err(|e| storage_err(None, e))?;

            if ids.is_empty() {
                return Ok(Vec::new());
            }

            let keys: Vec<String> = ids.iter().map(|id| Self::record_key(id)).collect();
            let values: Vec<Option<String>> = deadpool_redis::redis::cmd("MGET")
                .arg(&keys)
                .query_async(&mut conn)
                .await
                .map_err(|e| storage_err(None, e))?;

            let mut records = Vec::new();
            for raw in values.into_iter().flatten() {
                let record: SagaRecord =
                    serde_json::from_str(&raw).map_err(|e| storage_err(None, e))?;
                if !record.status.is_terminal() {
                    records.push(record);
                }
            }
            records.sort_by_key(|r| r.updated_at);
            Ok(records)
        })
    }

    fn try_acquire_execution(&self, saga_id: &str) -> BoxFuture<'_, Result<String, SagaError>> {
        let saga_id = saga_id.to_string();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| storage_err(Some(&saga_id), e))?;

            let token = uuid::Uuid::new_v4().to_string();
            let ttl_ms = u64::try_from(self.claim_ttl.as_millis().max(1)).unwrap_or(u64::MAX);

            let acquired: Option<String> = deadpool_redis::redis::cmd("SET")
                .arg(Self::claim_key(&saga_id))
                .arg(&token)
                .arg("NX")
                .arg("PX")
                .arg(ttl_ms)
                .query_async(&mut conn)
                .await
                .map_err(|e| storage_err(Some(&saga_id), e))?;

            if acquired.is_none() {
                return Err(SagaError::Storage {
                    saga_id: Some(saga_id),
                    message: "saga execution already claimed by another worker".into(),
                });
            }
            Ok(token)
        })
    }

    fn renew_execution(
        &self,
        saga_id: &str,
        claim_token: &str,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        let saga_id = saga_id.to_string();
        let claim_token = claim_token.to_string();
        let ttl_ms = u64::try_from(self.claim_ttl.as_millis().max(1)).unwrap_or(u64::MAX);
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| storage_err(Some(&saga_id), e))?;

            let renewed: i32 = Script::new(RENEW_CLAIM_LUA)
                .key(Self::claim_key(&saga_id))
                .arg(&claim_token)
                .arg(ttl_ms)
                .invoke_async(&mut conn)
                .await
                .map_err(|e| storage_err(Some(&saga_id), e))?;

            if renewed == 0 {
                return Err(SagaError::Storage {
                    saga_id: Some(saga_id),
                    message: "saga execution claim lost or expired".into(),
                });
            }
            Ok(())
        })
    }

    fn claim_ttl(&self) -> Option<Duration> {
        Some(self.claim_ttl)
    }

    fn release_execution(
        &self,
        saga_id: &str,
        claim_token: &str,
    ) -> BoxFuture<'_, Result<(), SagaError>> {
        let saga_id = saga_id.to_string();
        let claim_token = claim_token.to_string();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| storage_err(Some(&saga_id), e))?;

            let _: i32 = Script::new(RELEASE_CLAIM_LUA)
                .key(Self::claim_key(&saga_id))
                .arg(&claim_token)
                .invoke_async(&mut conn)
                .await
                .map_err(|e| storage_err(Some(&saga_id), e))?;

            Ok(())
        })
    }
}
