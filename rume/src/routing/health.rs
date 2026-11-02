//! Health checks.
//!
//! Provides concurrent health checking with per-check timeouts, optional
//! result caching (shared across clones via `Arc`), and the `/health/live`
//! and `/health/ready` HTTP routes.

use std::collections::{BTreeMap, HashSet};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{Router, get};
use serde::Serialize;
use serde_json::json;
use tokio::time::timeout;

use crate::app::{AppContext, AvailabilityState};

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
}

/// A health check that can be registered.
pub trait HealthCheck: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>>;
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
        let name = check.name().to_string();
        if !self.names.insert(name.clone()) {
            panic!("health check '{name}' is registered more than once");
        }
        self.checks.push(Arc::new(check));
        self
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
    /// When `Some(ttl)`, check results are cached and subsequent [`run`](Self::run)
    /// calls within the TTL return cached results (with `cached: true`) without
    /// re-running the checks. When `None`, caching is disabled.
    pub fn cache_ttl(mut self, ttl: Option<Duration>) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// Run all registered checks concurrently and produce a report.
    ///
    /// Each check is spawned on a task and wrapped in a per-check timeout.
    /// Results are aggregated: any [`HealthStatus::Failed`] → overall `Failed`
    /// (503 on `/health/ready`); any [`HealthStatus::Warning`] with no
    /// `Failed` → overall `Warning` (degraded, 503); all [`HealthStatus::Ok`]
    /// → overall `Ok` (200).
    pub async fn run(&self) -> HealthReport {
        // Serve from cache when a TTL is configured and every check has a
        // fresh cached result.
        if let Some(ttl) = self.cache_ttl {
            let now = Instant::now();
            let cache = self.cache.read().unwrap();
            let all_cached = !self.checks.is_empty()
                && self.checks.iter().all(|check| {
                    cache
                        .get(check.name())
                        .is_some_and(|(instant, _)| now.duration_since(*instant) < ttl)
                });
            drop(cache);
            if all_cached {
                let checks = self
                    .checks
                    .iter()
                    .map(|check| {
                        let mut result = self
                            .cache
                            .read()
                            .unwrap()
                            .get(check.name())
                            .expect("cached result present")
                            .1
                            .clone();
                        result.cached = true;
                        result
                    })
                    .collect::<Vec<_>>();
                return Self::build_report(checks);
            }
        }

        // Run all checks concurrently, each wrapped in a per-check timeout.
        let mut handles: Vec<(String, tokio::task::JoinHandle<HealthCheckResult>)> =
            Vec::with_capacity(self.checks.len());
        for check in &self.checks {
            let name = check.name().to_string();
            let check = Arc::clone(check);
            let timeout_duration = self.timeout;
            handles.push((
                name.clone(),
                tokio::spawn(async move {
                    match timeout(timeout_duration, check.run()).await {
                        Ok(result) => result,
                        Err(_elapsed) => HealthCheckResult {
                            name,
                            status: HealthStatus::Failed,
                            message: format!("health check timed out after {:?}", timeout_duration),
                            meta: BTreeMap::new(),
                            cached: false,
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
            }));
        }

        // Persist fresh results to the cache. TTL validity is enforced at
        // read time; we just record the instant now.
        if self.cache_ttl.is_some() {
            let mut cache = self.cache.write().unwrap();
            for result in &checks {
                cache.insert(result.name.clone(), (Instant::now(), result.clone()));
            }
        }

        Self::build_report(checks)
    }

    /// Aggregate check results into a [`HealthReport`].
    ///
    /// Any `Failed` → overall `Failed` (`"error"`, `is_healthy = false`);
    /// any `Warning` with no `Failed` → overall `Warning` (`"warning"`,
    /// `is_healthy = false`, degraded); all `Ok` → overall `Ok` (`"ok"`,
    /// `is_healthy = true`).
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
                    // Warning only upgrades "ok" to "warning"; a Failed check
                    // (already "error") takes precedence and wins overall.
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
    // S-m2: do not expose pid/platform in health responses — those are
    // operational internals that aid attackers. Return empty map; callers
    // that need debugging info can add it explicitly in development.
    BTreeMap::new()
}

/// Routes for health checks: `/health/live` and `/health/ready`.
///
/// - `GET /health/live` — liveness probe: returns 200 if the process is alive.
///   Does NOT check dependencies (a DB outage shouldn't trigger a restart).
/// - `GET /health/ready` — readiness probe: runs all registered health checks.
///   Returns 200 when healthy, 503 when any check fails.
///
/// Both routes use [`State<AppContext<E, C, S>>`] to access availability and
/// health checks from the application context.
pub fn health_routes<E, C, S>() -> Router<AppContext<E, C, S>>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/health/live",
            get(move |State(ctx): State<AppContext<E, C, S>>| async move {
                if ctx.availability.state() == AvailabilityState::Broken {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"status": "broken"})),
                    )
                        .into_response()
                } else {
                    (StatusCode::OK, Json(json!({"status": "alive"}))).into_response()
                }
            }),
        )
        .route(
            "/health/ready",
            get(move |State(ctx): State<AppContext<E, C, S>>| async move {
                // S-m4: readiness also respects lifecycle state — refuse
                // traffic when the application is terminating or
                // refusing traffic, even if all health checks pass.
                if ctx.availability.state() == AvailabilityState::RefusingTraffic
                    || ctx.availability.state() == AvailabilityState::Unknown
                    || ctx.availability.state() == AvailabilityState::Broken
                {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"status": "error"})),
                    )
                        .into_response();
                }
                let report = ctx.health_checks.run().await;

                // S-m5: the public readiness response is minimal and safe —
                // only the overall status. Detailed check results (including
                // `message` and `meta`, which may contain database errors,
                // infrastructure details, or credentials) are not exposed over
                // HTTP. Use `HealthChecks::run()` programmatically for full
                // diagnostics.
                let status = if report.is_healthy { "ok" } else { "error" };
                let code = if report.is_healthy {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                };
                (code, Json(json!({"status": status}))).into_response()
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{AppContext, Availability, ProjectPaths};
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    /// A check that always reports Ok.
    #[derive(Debug)]
    struct OkCheck;

    impl HealthCheck for OkCheck {
        fn name(&self) -> &str {
            "database"
        }
        fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
            Box::pin(async move {
                HealthCheckResult {
                    name: "database".to_string(),
                    status: HealthStatus::Ok,
                    message: "ok".to_string(),
                    meta: BTreeMap::new(),
                    cached: false,
                }
            })
        }
    }

    /// A check that always reports Failed.
    #[derive(Debug)]
    struct FailedCheck;

    impl HealthCheck for FailedCheck {
        fn name(&self) -> &str {
            "s3"
        }
        fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
            Box::pin(async move {
                HealthCheckResult {
                    name: "s3".to_string(),
                    status: HealthStatus::Failed,
                    message: "not found".to_string(),
                    meta: BTreeMap::new(),
                    cached: false,
                }
            })
        }
    }

    /// A check that delays before reporting Ok.
    #[derive(Debug)]
    struct TimingCheck {
        name: &'static str,
        delay: Duration,
        order: Arc<Mutex<Vec<String>>>,
    }

    impl HealthCheck for TimingCheck {
        fn name(&self) -> &str {
            self.name
        }
        fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
            let delay = self.delay;
            let name = self.name.to_string();
            let order = Arc::clone(&self.order);
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                order.lock().unwrap().push(name.clone());
                HealthCheckResult {
                    name,
                    status: HealthStatus::Ok,
                    message: "ok".to_string(),
                    meta: BTreeMap::new(),
                    cached: false,
                }
            })
        }
    }

    /// A check that counts how many times `run` is invoked.
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
            let runs = Arc::clone(&self.runs);
            let name = self.name.to_string();
            Box::pin(async move {
                runs.fetch_add(1, Ordering::SeqCst);
                HealthCheckResult {
                    name,
                    status: HealthStatus::Ok,
                    message: "ok".to_string(),
                    meta: BTreeMap::new(),
                    cached: false,
                }
            })
        }
    }

    /// A check that always reports Warning.
    #[derive(Debug)]
    struct WarningCheck;

    impl HealthCheck for WarningCheck {
        fn name(&self) -> &str {
            "cache"
        }
        fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
            Box::pin(async move {
                HealthCheckResult {
                    name: "cache".to_string(),
                    status: HealthStatus::Warning,
                    message: "high latency".to_string(),
                    meta: BTreeMap::new(),
                    cached: false,
                }
            })
        }
    }

    #[tokio::test]
    async fn health_checks_run_all_ok() {
        let checks = HealthChecks::new().register(OkCheck);
        let report = checks.run().await;
        assert!(report.is_healthy);
        assert_eq!(report.status, "ok");
    }

    #[tokio::test]
    async fn health_checks_run_failed_is_not_healthy() {
        let checks = HealthChecks::new().register(FailedCheck);
        let report = checks.run().await;
        assert!(!report.is_healthy);
        assert_eq!(report.status, "error");
    }

    #[tokio::test]
    async fn health_checks_run_warning_is_not_healthy_but_not_error() {
        let checks = HealthChecks::new().register(WarningCheck);
        let report = checks.run().await;
        assert!(!report.is_healthy);
        assert_eq!(report.status, "warning");
    }

    #[tokio::test]
    async fn health_checks_run_concurrently() {
        // A slow check must not block a fast check: both run concurrently.
        let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let checks = HealthChecks::new()
            .register(TimingCheck {
                name: "slow",
                delay: Duration::from_millis(150),
                order: Arc::clone(&order),
            })
            .register(TimingCheck {
                name: "fast",
                delay: Duration::from_millis(0),
                order: Arc::clone(&order),
            });

        let start = Instant::now();
        let report = checks.run().await;
        let elapsed = start.elapsed();

        assert!(report.is_healthy);
        // Fast check completed before the slow one → they ran concurrently,
        // not sequentially (sequential slow-first would finish slow first).
        let order = order.lock().unwrap();
        assert_eq!(order[0], "fast");
        // Concurrent wall-clock is bounded by the slowest check, not the sum.
        assert!(elapsed < Duration::from_millis(300));
    }

    #[tokio::test]
    async fn health_check_timeout_marks_failed() {
        // A check that sleeps longer than the per-check timeout is Failed.
        let checks = HealthChecks::new()
            .timeout(Duration::from_millis(50))
            .register(TimingCheck {
                name: "slow",
                delay: Duration::from_millis(250),
                order: Arc::new(Mutex::new(Vec::new())),
            });

        let report = checks.run().await;
        assert!(!report.is_healthy);
        assert_eq!(report.status, "error");

        let result = report
            .checks
            .iter()
            .find(|r| r.name == "slow")
            .expect("slow check");
        assert_eq!(result.status, HealthStatus::Failed);
        assert!(result.message.contains("timed out"));
        assert!(!result.cached);
    }

    #[tokio::test]
    async fn health_check_caching_returns_cached() {
        let runs = Arc::new(AtomicUsize::new(0));
        let checks = HealthChecks::new()
            .cache_ttl(Some(Duration::from_secs(60)))
            .register(CountingCheck {
                name: "db",
                runs: Arc::clone(&runs),
            });

        // First run executes the check.
        let report1 = checks.run().await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(!report1.checks[0].cached);

        // Second run within TTL returns cached results without re-running.
        let report2 = checks.run().await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(report2.checks.len(), 1);
        assert!(report2.checks[0].cached);
        assert_eq!(report2.checks[0].status, HealthStatus::Ok);
    }

    #[tokio::test]
    async fn health_check_cache_is_shared_across_clones() {
        // S-m3: cloned HealthChecks must share the same cache. Run on the
        // original, then run on the clone — the clone should see the cached
        // result (run count stays at 1).
        let runs = Arc::new(AtomicUsize::new(0));
        let original = HealthChecks::new()
            .cache_ttl(Some(Duration::from_secs(60)))
            .register(CountingCheck {
                name: "db",
                runs: Arc::clone(&runs),
            });

        let _ = original.run().await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        let clone = original.clone();
        let _ = clone.run().await;
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "clone should reuse shared cache"
        );
    }

    #[tokio::test]
    async fn health_check_degraded_when_warning() {
        // Warning without any Failed → overall Warning (degraded), not healthy.
        let checks = HealthChecks::new().register(WarningCheck);
        let report = checks.run().await;
        assert!(!report.is_healthy);
        assert_eq!(report.status, "warning");
        assert_eq!(report.checks[0].status, HealthStatus::Warning);
    }

    #[tokio::test]
    async fn health_routes_ready_respects_lifecycle() {
        // S-m4: readiness should report not-ready when availability is
        // refusing traffic or transitioning to termination.
        type TestEnv = std::collections::BTreeMap<String, String>;
        type TestConfig = std::collections::BTreeMap<String, String>;

        let ctx = AppContext::new(
            TestEnv::new(),
            TestConfig::new(),
            ProjectPaths::from_current_dir(),
            Availability::new(),
            HealthChecks::new().register(FailedCheck),
            (),
        );

        // Set availability to refusing traffic.
        ctx.availability.set(AvailabilityState::RefusingTraffic);

        // Build the health routes router and hit /health/ready.
        let router = super::health_routes::<TestEnv, TestConfig, ()>().with_state(ctx);

        let request = axum::http::Request::builder()
            .uri("/health/ready")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.expect("request failed");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// A check that includes sensitive data in its message and meta.
    #[derive(Debug)]
    struct SensitiveCheck;

    impl HealthCheck for SensitiveCheck {
        fn name(&self) -> &str {
            "database"
        }
        fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
            Box::pin(async move {
                HealthCheckResult {
                    name: "database".to_string(),
                    status: HealthStatus::Ok,
                    message: "connected to postgres://admin:secret@db.internal:5432/app"
                        .to_string(),
                    meta: BTreeMap::from([
                        (
                            "connection_string".to_string(),
                            serde_json::json!("postgres://admin:secret@db.internal:5432/app"),
                        ),
                        ("password".to_string(), serde_json::json!("secret")),
                    ]),
                    cached: false,
                }
            })
        }
    }

    #[tokio::test]
    #[should_panic(expected = "is registered more than once")]
    async fn health_check_duplicate_name_panics() {
        // S-m6: registering two checks with the same name must panic — the
        // cache keys on name, so a duplicate would let one result overwrite
        // the other and be reused for both checks.
        HealthChecks::new().register(OkCheck).register(OkCheck);
    }

    #[tokio::test]
    async fn health_check_duplicate_name_panics_with_message() {
        let result = std::panic::catch_unwind(|| {
            HealthChecks::new().register(OkCheck).register(OkCheck);
        });
        assert!(
            result.is_err(),
            "registering duplicate health check names must panic"
        );
    }

    #[tokio::test]
    async fn health_ready_returns_minimal_safe_response() {
        // S-m5: /health/ready returns only the overall status — no check
        // names, messages, or meta that could leak sensitive information.
        type TestEnv = std::collections::BTreeMap<String, String>;
        type TestConfig = std::collections::BTreeMap<String, String>;

        let ctx = AppContext::new(
            TestEnv::new(),
            TestConfig::new(),
            ProjectPaths::from_current_dir(),
            Availability::new(),
            HealthChecks::new().register(OkCheck),
            (),
        );
        ctx.availability.set(AvailabilityState::Ready);

        let router = super::health_routes::<TestEnv, TestConfig, ()>().with_state(ctx);

        let request = axum::http::Request::builder()
            .uri("/health/ready")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.expect("request failed");
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.get("status").and_then(|v| v.as_str()), Some("ok"));
        // Must not contain check-level details.
        assert!(json.get("checks").is_none());
        assert!(json.get("message").is_none());
        assert!(json.get("meta").is_none());
    }

    #[tokio::test]
    async fn health_ready_hides_sensitive_metadata() {
        // S-m5: even when a check reports sensitive data in message/meta,
        // /health/ready must not expose it.
        type TestEnv = std::collections::BTreeMap<String, String>;
        type TestConfig = std::collections::BTreeMap<String, String>;

        let ctx = AppContext::new(
            TestEnv::new(),
            TestConfig::new(),
            ProjectPaths::from_current_dir(),
            Availability::new(),
            HealthChecks::new().register(SensitiveCheck),
            (),
        );
        ctx.availability.set(AvailabilityState::Ready);

        let router = super::health_routes::<TestEnv, TestConfig, ()>().with_state(ctx);

        let request = axum::http::Request::builder()
            .uri("/health/ready")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.expect("request failed");
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(!text.contains("secret"));
        assert!(!text.contains("password"));
        assert!(!text.contains("connection_string"));
        assert!(!text.contains("postgres://"));
        // Only the minimal status field.
        assert!(text.contains("\"status\":\"ok\""));
    }

    #[tokio::test]
    async fn health_ready_returns_error_when_unhealthy() {
        type TestEnv = std::collections::BTreeMap<String, String>;
        type TestConfig = std::collections::BTreeMap<String, String>;

        let ctx = AppContext::new(
            TestEnv::new(),
            TestConfig::new(),
            ProjectPaths::from_current_dir(),
            Availability::new(),
            HealthChecks::new().register(FailedCheck),
            (),
        );
        ctx.availability.set(AvailabilityState::Ready);

        let router = super::health_routes::<TestEnv, TestConfig, ()>().with_state(ctx);

        let request = axum::http::Request::builder()
            .uri("/health/ready")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.expect("request failed");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.get("status").and_then(|v| v.as_str()), Some("error"));
        assert!(json.get("checks").is_none());
    }

    #[tokio::test]
    async fn health_ready_concurrent_requests() {
        // Multiple concurrent /health/ready requests must all get consistent
        // results without panicking or deadlocking.
        type TestEnv = std::collections::BTreeMap<String, String>;
        type TestConfig = std::collections::BTreeMap<String, String>;

        let ctx = AppContext::new(
            TestEnv::new(),
            TestConfig::new(),
            ProjectPaths::from_current_dir(),
            Availability::new(),
            HealthChecks::new().register(OkCheck),
            (),
        );
        ctx.availability.set(AvailabilityState::Ready);

        let router = super::health_routes::<TestEnv, TestConfig, ()>().with_state(ctx);

        let mut handles = Vec::new();
        for _ in 0..10 {
            let r = router.clone();
            handles.push(tokio::spawn(async move {
                let request = axum::http::Request::builder()
                    .uri("/health/ready")
                    .body(axum::body::Body::empty())
                    .unwrap();
                r.oneshot(request).await.expect("request failed")
            }));
        }

        for handle in handles {
            let response = handle.await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn health_check_caching_avoids_rerun_with_ttl() {
        let runs = Arc::new(AtomicUsize::new(0));
        let checks = HealthChecks::new()
            .cache_ttl(Some(Duration::from_secs(60)))
            .register(CountingCheck {
                name: "db",
                runs: Arc::clone(&runs),
            });

        let _ = checks.run().await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        let _ = checks.run().await;
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "second run within TTL must not re-execute"
        );
    }
}
