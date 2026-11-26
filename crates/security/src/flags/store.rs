//! Feature flag store trait and in-memory implementation.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::RwLock;

use thiserror::Error;

use super::eval::{EvaluationContext, FeatureFlag, evaluate_flag};

/// Pinned boxed future returned by async [`FeatureFlagStore`] operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Errors returned by [`FeatureFlagStore`] implementations.
#[derive(Debug, Error)]
pub enum FlagError {
    /// Underlying storage error.
    #[error("feature flag store error: {0}")]
    Store(String),

    /// JSON serialization/deserialization error for targeting rules.
    #[error("feature flag serialization error: {0}")]
    Serialization(String),
}

/// Persistent or in-memory feature flag repository and evaluator.
pub trait FeatureFlagStore: Send + Sync + 'static {
    /// Inserts or updates a feature flag.
    fn upsert_flag<'a>(&'a self, flag: FeatureFlag) -> BoxFuture<'a, Result<(), FlagError>>;

    /// Retrieves a flag by `key` and optional `tenant_id` scope.
    fn get_flag<'a>(
        &'a self,
        key: &'a str,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<FeatureFlag>, FlagError>>;

    /// Lists all flags for `tenant_id` (or global flags when `tenant_id` is `None`).
    fn list_flags<'a>(
        &'a self,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<FeatureFlag>, FlagError>>;

    /// Deletes a flag by `key` and optional `tenant_id`, returning `true` if one was removed.
    fn delete_flag<'a>(
        &'a self,
        key: &'a str,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<bool, FlagError>>;

    /// Evaluates `key` for `ctx`, checking tenant-scoped override first and
    /// falling back to the global flag. Returns `false` if no flag exists.
    fn evaluate<'a>(
        &'a self,
        key: &'a str,
        ctx: &'a EvaluationContext,
    ) -> BoxFuture<'a, Result<bool, FlagError>>;
}

/// Thread-safe in-memory [`FeatureFlagStore`].
#[derive(Debug, Default)]
pub struct InMemoryFeatureFlags {
    flags: RwLock<HashMap<(Option<String>, String), FeatureFlag>>,
}

impl InMemoryFeatureFlags {
    /// Creates an empty in-memory feature flag store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl FeatureFlagStore for InMemoryFeatureFlags {
    fn upsert_flag<'a>(&'a self, flag: FeatureFlag) -> BoxFuture<'a, Result<(), FlagError>> {
        Box::pin(async move {
            let key = (flag.tenant_id.clone(), flag.key.clone());
            self.flags
                .write()
                .map_err(|e| FlagError::Store(format!("lock poisoned: {e}")))?
                .insert(key, flag);
            Ok(())
        })
    }

    fn get_flag<'a>(
        &'a self,
        key: &'a str,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<FeatureFlag>, FlagError>> {
        Box::pin(async move {
            let guard = self
                .flags
                .read()
                .map_err(|e| FlagError::Store(format!("lock poisoned: {e}")))?;
            Ok(guard
                .get(&(tenant_id.map(str::to_string), key.to_string()))
                .cloned())
        })
    }

    fn list_flags<'a>(
        &'a self,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<FeatureFlag>, FlagError>> {
        Box::pin(async move {
            let guard = self
                .flags
                .read()
                .map_err(|e| FlagError::Store(format!("lock poisoned: {e}")))?;
            let mut flags: Vec<FeatureFlag> = guard
                .values()
                .filter(|f| f.tenant_id.as_deref() == tenant_id)
                .cloned()
                .collect();
            flags.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(flags)
        })
    }

    fn delete_flag<'a>(
        &'a self,
        key: &'a str,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<bool, FlagError>> {
        Box::pin(async move {
            let mut guard = self
                .flags
                .write()
                .map_err(|e| FlagError::Store(format!("lock poisoned: {e}")))?;
            Ok(guard
                .remove(&(tenant_id.map(str::to_string), key.to_string()))
                .is_some())
        })
    }

    fn evaluate<'a>(
        &'a self,
        key: &'a str,
        ctx: &'a EvaluationContext,
    ) -> BoxFuture<'a, Result<bool, FlagError>> {
        Box::pin(async move {
            let guard = self
                .flags
                .read()
                .map_err(|e| FlagError::Store(format!("lock poisoned: {e}")))?;
            if let Some(tid) = ctx.tenant_id.as_deref()
                && let Some(flag) = guard.get(&(Some(tid.to_string()), key.to_string()))
            {
                return Ok(evaluate_flag(flag, ctx));
            }
            if let Some(flag) = guard.get(&(None, key.to_string())) {
                return Ok(evaluate_flag(flag, ctx));
            }
            Ok(false)
        })
    }
}
