//! Usage metering traits, records, and in-memory implementation.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Pinned boxed future returned by async [`UsageMeter`] and [`BillingSink`] operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Errors returned by [`UsageMeter`] and [`BillingSink`] operations.
#[derive(Debug, Error)]
pub enum MeteringError {
    /// Underlying storage error.
    #[error("usage metering store error: {0}")]
    Store(String),

    /// Billing sink export failure.
    #[error("billing sink export error: {0}")]
    BillingSink(String),
}

/// Aggregated usage record exported to a [`BillingSink`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRecord {
    /// Tenant identifier.
    pub tenant_id: String,
    /// Metered metric name (e.g. `"api_requests"`, `"storage_bytes"`).
    pub metric: String,
    /// Aggregated quantity since the last flush.
    pub quantity: u64,
    /// Unix epoch timestamp in seconds when the batch was recorded/flushed.
    pub recorded_at: u64,
}

/// Downstream billing exporter invoked by [`UsageMeter::flush`].
pub trait BillingSink: Send + Sync + 'static {
    /// Exports a batch of aggregated usage records.
    fn export_batch<'a>(
        &'a self,
        records: &'a [UsageRecord],
    ) -> BoxFuture<'a, Result<(), MeteringError>>;
}

/// Tenant-scoped usage metering contract.
pub trait UsageMeter: Send + Sync + 'static {
    /// Increments the usage counter for `(tenant_id, metric)` by `quantity`.
    fn record_usage<'a>(
        &'a self,
        tenant_id: &'a str,
        metric: &'a str,
        quantity: u64,
    ) -> BoxFuture<'a, Result<(), MeteringError>>;

    /// Returns the current un-flushed usage total for `(tenant_id, metric)`.
    fn get_usage<'a>(
        &'a self,
        tenant_id: &'a str,
        metric: &'a str,
    ) -> BoxFuture<'a, Result<u64, MeteringError>>;

    /// Exports all non-zero counters to `sink` and checkpoints/resets the
    /// exported quantities, returning the number of records exported.
    fn flush<'a>(
        &'a self,
        sink: &'a dyn BillingSink,
    ) -> BoxFuture<'a, Result<usize, MeteringError>>;
}

pub(crate) fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Thread-safe in-memory [`UsageMeter`].
#[derive(Debug, Default)]
pub struct InMemoryUsageMeter {
    counters: Mutex<BTreeMap<(String, String), u64>>,
}

impl InMemoryUsageMeter {
    /// Creates an empty in-memory usage meter.
    pub fn new() -> Self {
        Self::default()
    }
}

impl UsageMeter for InMemoryUsageMeter {
    fn record_usage<'a>(
        &'a self,
        tenant_id: &'a str,
        metric: &'a str,
        quantity: u64,
    ) -> BoxFuture<'a, Result<(), MeteringError>> {
        Box::pin(async move {
            if quantity == 0 {
                return Ok(());
            }
            let mut guard = self
                .counters
                .lock()
                .map_err(|e| MeteringError::Store(format!("lock poisoned: {e}")))?;
            let entry = guard
                .entry((tenant_id.to_string(), metric.to_string()))
                .or_insert(0);
            *entry = entry.saturating_add(quantity);
            Ok(())
        })
    }

    fn get_usage<'a>(
        &'a self,
        tenant_id: &'a str,
        metric: &'a str,
    ) -> BoxFuture<'a, Result<u64, MeteringError>> {
        Box::pin(async move {
            let guard = self
                .counters
                .lock()
                .map_err(|e| MeteringError::Store(format!("lock poisoned: {e}")))?;
            Ok(guard
                .get(&(tenant_id.to_string(), metric.to_string()))
                .copied()
                .unwrap_or(0))
        })
    }

    fn flush<'a>(
        &'a self,
        sink: &'a dyn BillingSink,
    ) -> BoxFuture<'a, Result<usize, MeteringError>> {
        Box::pin(async move {
            let now = current_epoch_secs();
            let records: Vec<UsageRecord> = {
                let guard = self
                    .counters
                    .lock()
                    .map_err(|e| MeteringError::Store(format!("lock poisoned: {e}")))?;
                guard
                    .iter()
                    .filter(|&(_, &qty)| qty > 0)
                    .map(|((tenant_id, metric), &quantity)| UsageRecord {
                        tenant_id: tenant_id.clone(),
                        metric: metric.clone(),
                        quantity,
                        recorded_at: now,
                    })
                    .collect()
            };

            if records.is_empty() {
                return Ok(0);
            }

            // Export before checkpointing so a sink failure never drops usage.
            sink.export_batch(&records).await?;

            let mut guard = self
                .counters
                .lock()
                .map_err(|e| MeteringError::Store(format!("lock poisoned: {e}")))?;
            for rec in &records {
                let key = (rec.tenant_id.clone(), rec.metric.clone());
                if let Some(current) = guard.get_mut(&key) {
                    *current = current.saturating_sub(rec.quantity);
                    if *current == 0 {
                        guard.remove(&key);
                    }
                }
            }

            Ok(records.len())
        })
    }
}
