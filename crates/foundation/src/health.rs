//! Health check contracts and concurrent runner.
//!
//! Provides concurrent health checking with per-check timeouts, optional
//! result caching (shared across clones via `Arc`), and singleflight
//! stampede suppression.

use std::collections::{BTreeMap, HashSet};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::time::timeout;

/// Health check result status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Ok,
    Warning,
    Failed,
}

/// A single health check result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthCheckResult {
    pub name: String,
    pub status: HealthStatus,
    pub message: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
    pub cached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl HealthCheckResult {
    /// Creates a healthy result.
    pub fn ok(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: HealthStatus::Ok,
            message: message.into(),
            meta: BTreeMap::new(),
            cached: false,
            duration_ms: None,
        }
    }

    /// Creates a warning result.
    pub fn warning(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: HealthStatus::Warning,
            message: message.into(),
            meta: BTreeMap::new(),
            cached: false,
            duration_ms: None,
        }
    }

    /// Creates a failed result.
    pub fn failed(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: HealthStatus::Failed,
            message: message.into(),
            meta: BTreeMap::new(),
            cached: false,
            duration_ms: None,
        }
    }

    /// Attaches metadata to the health check result.
    pub fn with_meta(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.meta.insert(key.into(), value.into());
        self
    }

    /// Attaches execution duration in milliseconds.
    pub fn with_duration_ms(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }
}

/// A health check that can be registered.
pub trait HealthCheck: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>>;
}

/// Observer notified after each health check executes.
///
/// Why: Keeps `rume-foundation` independent of `rume-observability` while
/// allowing metrics registries to record health check latency and status.
pub trait HealthCheckObserver: Send + Sync + std::fmt::Debug {
    fn record_health_check(&self, name: &str, status: HealthStatus, duration: Duration);
}

/// A full health report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthReport {
    pub is_healthy: bool,
    pub status: String,
    pub finished_at: String,
    pub debug_info: BTreeMap<String, serde_json::Value>,
    pub checks: Vec<HealthCheckResult>,
}

/// Default per-check timeout: 5 seconds.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Registry of health checks.
///
/// Checks run concurrently on each [`run`](Self::run) call. Each check is
/// wrapped in a [`tokio::time::timeout`]; a check that exceeds the timeout is
/// marked [`HealthStatus::Failed`]. When a `cache_ttl` is set, results are
/// cached internally behind an [`Arc`]`<`[`RwLock`]`<...>>` so that cloned
/// [`HealthChecks`] share the same application-wide cache rather than each
/// getting an independent copy.
#[derive(Debug)]
pub struct HealthChecks {
    checks: Vec<Arc<dyn HealthCheck>>,
    /// Track registered check names to reject duplicates.
    names: HashSet<String>,
    timeout: Duration,
    cache_ttl: Option<Duration>,
    cache: Arc<RwLock<BTreeMap<String, (Instant, HealthCheckResult)>>>,
    singleflight:
        Arc<tokio::sync::Mutex<Option<tokio::sync::watch::Receiver<Option<HealthReport>>>>>,
    observer: Option<Arc<dyn HealthCheckObserver>>,
}

impl HealthChecks {
    /// Create a new empty health checks registry.
    ///
    /// Default timeout is [`DEFAULT_TIMEOUT`] (5s) and caching is disabled.
    pub fn new() -> Self {
        Self {
            checks: Vec::new(),
            names: HashSet::new(),
            timeout: DEFAULT_TIMEOUT,
            cache_ttl: None,
            cache: Arc::new(RwLock::new(BTreeMap::new())),
            singleflight: Arc::new(tokio::sync::Mutex::new(None)),
            observer: None,
        }
    }

    /// Register a health check.
    ///
    /// # Panics
    ///
    /// Panics if a check with the same name is already registered. Health
    /// check names must be unique because the result cache keys on name —
    /// a duplicate name would let one check's result overwrite another's.
    pub fn register(mut self, check: impl HealthCheck + 'static) -> Self {
        self.add(check);
        self
    }

    /// Add a health check by mutable reference.
    ///
    /// # Panics
    ///
    /// Panics if a check with the same name is already registered.
    pub fn add(&mut self, check: impl HealthCheck + 'static) {
        let name = check.name().to_string();
        if !self.names.insert(name.clone()) {
            panic!("health check '{name}' is registered more than once");
        }
        self.checks.push(Arc::new(check));
    }

    /// Set the per-check timeout. A check that does not complete within this
    /// duration is marked [`HealthStatus::Failed`].
    ///
    /// Default: [`DEFAULT_TIMEOUT`] (5 seconds).
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = duration;
        self
    }

    /// Set the optional result cache TTL.
    ///
    /// When `Some(ttl)`, per-check results are cached at completion time and
    /// subsequent [`run`](Self::run) calls return cached results (`cached: true`)
    /// when every registered check has an entry strictly younger than `ttl`
    /// (`elapsed < ttl`, so `Duration::ZERO` or an empty registry never hits the
    /// cache). Concurrent callers share a singleflight execution where the
    /// leader returns `cached: false` and followers read the cached entries.
    /// When `None`, caching and singleflight suppression are disabled.
    pub fn cache_ttl(mut self, ttl: Option<Duration>) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// Attaches a [`HealthCheckObserver`] so executed health checks
    /// automatically record latency and status.
    pub fn with_observer(mut self, observer: Arc<dyn HealthCheckObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Retrieves a cached report if the registry is non-empty and every
    /// registered check has a fresh result strictly within `ttl` (`elapsed < ttl`),
    /// marking each returned check with `cached: true`.
    fn get_cached_report(&self, ttl: Duration) -> Option<HealthReport> {
        let now = Instant::now();
        let cache = self.cache.read().unwrap();
        if self.checks.is_empty() {
            return None;
        }
        let all_cached = self.checks.iter().all(|check| {
            cache
                .get(check.name())
                .is_some_and(|(instant, _)| now.duration_since(*instant) < ttl)
        });
        if all_cached {
            let checks = self
                .checks
                .iter()
                .map(|check| {
                    let mut result = cache
                        .get(check.name())
                        .expect("cached result present")
                        .1
                        .clone();
                    result.cached = true;
                    result
                })
                .collect::<Vec<_>>();
            Some(Self::build_report(checks))
        } else {
            None
        }
    }

    /// Run all checks concurrently without cache interaction.
    async fn execute_checks(&self) -> HealthReport {
        let mut handles: Vec<(String, tokio::task::JoinHandle<HealthCheckResult>)> =
            Vec::with_capacity(self.checks.len());
        for check in &self.checks {
            let name = check.name().to_string();
            let check = Arc::clone(check);
            let timeout_duration = self.timeout;
            handles.push((
                name.clone(),
                tokio::spawn(async move {
                    let start = Instant::now();
                    match timeout(timeout_duration, check.run()).await {
                        Ok(mut result) => {
                            if result.duration_ms.is_none() {
                                result.duration_ms = Some(start.elapsed().as_millis() as u64);
                            }
                            result
                        }
                        Err(_elapsed) => HealthCheckResult {
                            name,
                            status: HealthStatus::Failed,
                            message: format!("health check timed out after {:?}", timeout_duration),
                            meta: BTreeMap::new(),
                            cached: false,
                            duration_ms: Some(start.elapsed().as_millis() as u64),
                        },
                    }
                }),
            ));
        }

        let mut checks = Vec::with_capacity(handles.len());
        for (name, handle) in handles {
            // A join error means the check task itself panicked; treat as Failed.
            checks.push(handle.await.unwrap_or_else(move |_| HealthCheckResult {
                name,
                status: HealthStatus::Failed,
                message: "health check task panicked".to_string(),
                meta: BTreeMap::new(),
                cached: false,
                duration_ms: None,
            }));
        }

        if let Some(observer) = &self.observer {
            for check in &checks {
                let duration = Duration::from_millis(check.duration_ms.unwrap_or(0));
                observer.record_health_check(&check.name, check.status.clone(), duration);
            }
        }

        Self::build_report(checks)
    }

    /// Run all registered checks concurrently and produce a report.
    ///
    /// Each check is spawned on a task and wrapped in a per-check timeout.
    /// Results are aggregated: any [`HealthStatus::Failed`] → overall `"error"`
    /// (503 on `/health/ready`); any [`HealthStatus::Warning`] with no
    /// `Failed` → overall `"warning"` (degraded, 503); all [`HealthStatus::Ok`]
    /// → overall `"ok"` (200).
    ///
    /// When `cache_ttl` is enabled, concurrent requests are protected by
    /// singleflight stampede suppression to prevent probe herds.
    pub async fn run(&self) -> HealthReport {
        if let Some(ttl) = self.cache_ttl {
            if let Some(report) = self.get_cached_report(ttl) {
                return report;
            }

            // Singleflight stampede suppression
            loop {
                let (waiter, leader_tx) = {
                    let mut sf = self.singleflight.lock().await;
                    // Re-check under lock in case another probe just completed
                    if let Some(report) = self.get_cached_report(ttl) {
                        return report;
                    }
                    if let Some(existing) = &*sf {
                        (Some(existing.clone()), None)
                    } else {
                        let (tx, rx) = tokio::sync::watch::channel(None);
                        *sf = Some(rx);
                        (None, Some(tx))
                    }
                };

                if let Some(mut rx) = waiter {
                    if rx.wait_for(|val| val.is_some()).await.is_err() {
                        let mut sf = self.singleflight.lock().await;
                        *sf = None;
                        continue;
                    }
                    if let Some(report) = self.get_cached_report(ttl) {
                        return report;
                    }
                    continue;
                }

                // Leader executes the checks
                let report = self.execute_checks().await;

                // Persist fresh results
                {
                    let mut cache = self.cache.write().unwrap();
                    for result in &report.checks {
                        cache.insert(result.name.clone(), (Instant::now(), result.clone()));
                    }
                }

                // Publish to waiters BEFORE clearing the slot, so a follower
                // that subscribes between the publish and the clear still
                // observes the latest value via wait_for().
                if let Some(tx) = leader_tx {
                    let _ = tx.send(Some(report.clone()));
                }

                // Clear singleflight slot after publishing.
                {
                    let mut sf = self.singleflight.lock().await;
                    *sf = None;
                }

                return report;
            }
        }

        self.execute_checks().await
    }

    /// Aggregate check results into a [`HealthReport`].
    fn build_report(checks: Vec<HealthCheckResult>) -> HealthReport {
        let mut is_healthy = true;
        let mut status = "ok".to_string();

        for result in &checks {
            let healthy = matches!(result.status, HealthStatus::Ok);
            if !healthy {
                is_healthy = false;
            }
            match result.status {
                HealthStatus::Ok => {}
                HealthStatus::Warning => {
                    if status == "ok" {
                        status = "warning".to_string();
                    }
                }
                HealthStatus::Failed => status = "error".to_string(),
            }
        }

        HealthReport {
            is_healthy,
            status,
            finished_at: current_timestamp(),
            debug_info: debug_info(),
            checks,
        }
    }
}

impl Clone for HealthChecks {
    fn clone(&self) -> Self {
        Self {
            checks: self.checks.clone(),
            names: self.names.clone(),
            timeout: self.timeout,
            cache_ttl: self.cache_ttl,
            cache: Arc::clone(&self.cache),
            singleflight: Arc::clone(&self.singleflight),
            observer: self.observer.clone(),
        }
    }
}

impl Default for HealthChecks {
    fn default() -> Self {
        Self::new()
    }
}

fn current_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

fn debug_info() -> BTreeMap<String, serde_json::Value> {
    BTreeMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct StaticCheck {
        name: &'static str,
        status: HealthStatus,
    }

    impl HealthCheck for StaticCheck {
        fn name(&self) -> &str {
            self.name
        }

        fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
            let name = self.name.to_string();
            let status = self.status.clone();
            Box::pin(async move {
                HealthCheckResult {
                    name,
                    status,
                    message: "checked".to_string(),
                    meta: BTreeMap::new(),
                    cached: false,
                    duration_ms: None,
                }
            })
        }
    }

    #[tokio::test]
    async fn foundation_health_checks_aggregates_worst_status() {
        let ok_only = HealthChecks::new().register(StaticCheck {
            name: "db",
            status: HealthStatus::Ok,
        });
        let report = ok_only.run().await;
        assert!(report.is_healthy);
        assert_eq!(report.status, "ok");

        let warned = HealthChecks::new()
            .register(StaticCheck {
                name: "db",
                status: HealthStatus::Ok,
            })
            .register(StaticCheck {
                name: "cache",
                status: HealthStatus::Warning,
            });
        let report = warned.run().await;
        assert!(!report.is_healthy);
        assert_eq!(report.status, "warning");

        let failed = HealthChecks::new()
            .register(StaticCheck {
                name: "db",
                status: HealthStatus::Warning,
            })
            .register(StaticCheck {
                name: "queue",
                status: HealthStatus::Failed,
            });
        let report = failed.run().await;
        assert!(!report.is_healthy);
        assert_eq!(report.status, "error");
    }

    #[tokio::test]
    async fn health_check_cache_ttl_edge_cases_match_contract() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Debug)]
        struct CountingCheck {
            name: &'static str,
            runs: Arc<AtomicUsize>,
        }

        impl HealthCheck for CountingCheck {
            fn name(&self) -> &str {
                self.name
            }

            fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
                let name = self.name.to_string();
                let runs = Arc::clone(&self.runs);
                Box::pin(async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    HealthCheckResult {
                        name,
                        status: HealthStatus::Ok,
                        message: "ok".to_string(),
                        meta: BTreeMap::new(),
                        cached: false,
                        duration_ms: None,
                    }
                })
            }
        }

        // Empty registry never hits cache even when cache_ttl is set.
        let empty = HealthChecks::new().cache_ttl(Some(Duration::from_secs(60)));
        assert!(empty.get_cached_report(Duration::from_secs(60)).is_none());
        let empty_report = empty.run().await;
        assert!(empty_report.checks.is_empty());

        // Duration::ZERO never returns a cached result because elapsed < ZERO is always false.
        let zero_runs = Arc::new(AtomicUsize::new(0));
        let zero_ttl = HealthChecks::new()
            .cache_ttl(Some(Duration::ZERO))
            .register(CountingCheck {
                name: "db",
                runs: Arc::clone(&zero_runs),
            });
        let r1 = zero_ttl.run().await;
        let r2 = zero_ttl.run().await;
        assert_eq!(zero_runs.load(Ordering::SeqCst), 2);
        assert!(!r1.checks[0].cached);
        assert!(!r2.checks[0].cached);

        // Positive TTL returns cached=true within TTL and re-runs once expired.
        let runs = Arc::new(AtomicUsize::new(0));
        let short_ttl = HealthChecks::new()
            .cache_ttl(Some(Duration::from_millis(40)))
            .register(CountingCheck {
                name: "db",
                runs: Arc::clone(&runs),
            });
        let first = short_ttl.run().await;
        assert!(!first.checks[0].cached);
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        let cached = short_ttl.run().await;
        assert!(cached.checks[0].cached);
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        // Backdate the cached timestamp past TTL instead of sleeping so expiry is deterministic.
        {
            let mut cache = short_ttl.cache.write().unwrap();
            let entry = cache.get_mut("db").expect("cached entry for db");
            entry.0 = Instant::now() - Duration::from_secs(10);
        }
        let expired = short_ttl.run().await;
        assert!(!expired.checks[0].cached);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }
}
