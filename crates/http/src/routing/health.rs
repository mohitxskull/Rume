//! Health checks.
//!
//! Provides concurrent health checking with per-check timeouts, optional
//! result caching (shared across clones via `Arc`), and the `/health/live`
//! and `/health/ready` HTTP routes.

use axum::extract::{FromRef, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use rume_foundation::state::{Availability, AvailabilityState};
use serde_json::json;

use crate::routing::Router;

pub use rume_foundation::health::{
    HealthCheck, HealthCheckObserver, HealthCheckResult, HealthChecks, HealthReport, HealthStatus,
};

/// Minimal state carrying [`Availability`] and [`HealthChecks`] for [`health_routes`].
#[derive(Debug, Clone)]
pub struct HealthRoutesState {
    pub availability: Availability,
    pub health_checks: HealthChecks,
}

impl FromRef<HealthRoutesState> for Availability {
    fn from_ref(state: &HealthRoutesState) -> Self {
        state.availability.clone()
    }
}

impl FromRef<HealthRoutesState> for HealthChecks {
    fn from_ref(state: &HealthRoutesState) -> Self {
        state.health_checks.clone()
    }
}

/// Routes for health checks: `/health/live` and `/health/ready`.
///
/// - `GET /health/live` — liveness probe: returns 200 if the process is alive.
///   Does NOT check dependencies (a DB outage shouldn't trigger a restart).
/// - `GET /health/ready` — readiness probe: runs all registered health checks.
///   Returns 200 when healthy, 503 when any check fails.
///
/// Both routes extract [`Availability`] and [`HealthChecks`] via [`FromRef`].
pub fn health_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    Availability: FromRef<S>,
    HealthChecks: FromRef<S>,
{
    Router::new()
        .get(
            "/health/live",
            move |State(availability): State<Availability>| async move {
                let state = availability.state();
                if state == AvailabilityState::Broken || state == AvailabilityState::Unknown {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"status": "broken"})),
                    )
                        .into_response()
                } else {
                    (StatusCode::OK, Json(json!({"status": "alive"}))).into_response()
                }
            },
        )
        .get(
            "/health/ready",
            move |State(availability): State<Availability>,
                  State(health_checks): State<HealthChecks>| async move {
                // Routing C1: readiness strictly requires AvailabilityState::Ready.
                // Refuse traffic when the application is booting (Live), terminating
                // (RefusingTraffic), Broken, or Unknown.
                if availability.state() != AvailabilityState::Ready {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"status": "error"})),
                    )
                        .into_response();
                }
                let report = health_checks.run().await;

                // S-m5: the public readiness response is minimal and safe —
                // only the overall status. Detailed check results (including
                // `message` and `meta`, which may contain database errors,
                // infrastructure details, or credentials) are not exposed over
                // HTTP. Use `HealthChecks::run()` programmatically for full
                // diagnostics.
                let status = if report.is_healthy {
                    "ok"
                } else {
                    &report.status
                };
                let code = if report.is_healthy {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                };
                (code, Json(json!({"status": status}))).into_response()
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
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
                    duration_ms: None,
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
                    duration_ms: None,
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
                    duration_ms: None,
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
                    duration_ms: None,
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
                    duration_ms: None,
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

    #[tokio::test(start_paused = true)]
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

    #[tokio::test(start_paused = true)]
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
        let ctx = HealthRoutesState {
            availability: Availability::new(),
            health_checks: HealthChecks::new().register(FailedCheck),
        };

        // Set availability to refusing traffic.
        ctx.availability.set(AvailabilityState::RefusingTraffic);

        // Build the health routes router and hit /health/ready.
        let router = super::health_routes::<HealthRoutesState>().with_state(ctx);

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
                    duration_ms: None,
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
        let ctx = HealthRoutesState {
            availability: Availability::new(),
            health_checks: HealthChecks::new().register(OkCheck),
        };
        ctx.availability.set(AvailabilityState::Ready);

        let router = super::health_routes::<HealthRoutesState>().with_state(ctx);

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
        let ctx = HealthRoutesState {
            availability: Availability::new(),
            health_checks: HealthChecks::new().register(SensitiveCheck),
        };
        ctx.availability.set(AvailabilityState::Ready);

        let router = super::health_routes::<HealthRoutesState>().with_state(ctx);

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
        let ctx = HealthRoutesState {
            availability: Availability::new(),
            health_checks: HealthChecks::new().register(FailedCheck),
        };
        ctx.availability.set(AvailabilityState::Ready);

        let router = super::health_routes::<HealthRoutesState>().with_state(ctx);

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
        let ctx = HealthRoutesState {
            availability: Availability::new(),
            health_checks: HealthChecks::new().register(OkCheck),
        };
        ctx.availability.set(AvailabilityState::Ready);

        let router = super::health_routes::<HealthRoutesState>().with_state(ctx);

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

    #[tokio::test]
    async fn health_ready_strictly_requires_ready_state_rejecting_live() {
        let ctx = HealthRoutesState {
            availability: Availability::new(),
            health_checks: HealthChecks::new().register(OkCheck),
        };

        // App starts in Live state during startup/migrations
        ctx.availability.set(AvailabilityState::Live);

        let router = super::health_routes::<HealthRoutesState>().with_state(ctx.clone());

        let request = axum::http::Request::builder()
            .uri("/health/ready")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router
            .clone()
            .oneshot(request)
            .await
            .expect("request failed");
        // Must reject with 503 during Live/startup phase
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        // Once transitioned to Ready, /health/ready returns 200 OK
        ctx.availability.set(AvailabilityState::Ready);
        let request2 = axum::http::Request::builder()
            .uri("/health/ready")
            .body(axum::body::Body::empty())
            .unwrap();
        let response2 = router.oneshot(request2).await.expect("request failed");
        assert_eq!(response2.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn health_live_rejects_unknown_and_broken() {
        let ctx = HealthRoutesState {
            availability: Availability::new(),
            health_checks: HealthChecks::new(),
        };

        let router = super::health_routes::<HealthRoutesState>().with_state(ctx.clone());

        // Default state is Unknown -> must return 503
        let request = axum::http::Request::builder()
            .uri("/health/live")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router
            .clone()
            .oneshot(request)
            .await
            .expect("request failed");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        // Live state -> 200 OK
        ctx.availability.set(AvailabilityState::Live);
        let request = axum::http::Request::builder()
            .uri("/health/live")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router
            .clone()
            .oneshot(request)
            .await
            .expect("request failed");
        assert_eq!(response.status(), StatusCode::OK);

        // Ready state -> 200 OK
        ctx.availability.set(AvailabilityState::Ready);
        let request = axum::http::Request::builder()
            .uri("/health/live")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router
            .clone()
            .oneshot(request)
            .await
            .expect("request failed");
        assert_eq!(response.status(), StatusCode::OK);

        // Broken state -> 503
        ctx.availability.set(AvailabilityState::Broken);
        let request = axum::http::Request::builder()
            .uri("/health/live")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.expect("request failed");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test(start_paused = true)]
    async fn test_health_checks_singleflight_stampede_protection() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Debug)]
        struct CountingCheck {
            runs: Arc<AtomicUsize>,
        }

        impl HealthCheck for CountingCheck {
            fn name(&self) -> &str {
                "counting"
            }
            fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
                let runs = Arc::clone(&self.runs);
                Box::pin(async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    HealthCheckResult {
                        name: "counting".to_string(),
                        status: HealthStatus::Ok,
                        message: "ok".to_string(),
                        meta: BTreeMap::new(),
                        cached: false,
                        duration_ms: None,
                    }
                })
            }
        }

        let runs = Arc::new(AtomicUsize::new(0));
        let hc = Arc::new(
            HealthChecks::new()
                .cache_ttl(Some(Duration::from_secs(60)))
                .register(CountingCheck {
                    runs: Arc::clone(&runs),
                }),
        );

        let mut handles = Vec::new();
        for _ in 0..10 {
            let hc_clone = Arc::clone(&hc);
            handles.push(tokio::spawn(async move { hc_clone.run().await }));
        }

        let mut cached_count = 0;
        for handle in handles {
            let report = handle.await.unwrap();
            assert!(report.is_healthy);
            if report.checks[0].cached {
                cached_count += 1;
            }
        }

        // Exactly 1 execution occurred despite 10 concurrent requests
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // Followers observed cached: true
        assert_eq!(cached_count, 9);
    }

    #[tokio::test]
    async fn health_check_populates_duration_ms() {
        let hc = HealthChecks::new().register(OkCheck);
        let report = hc.run().await;
        assert_eq!(report.checks.len(), 1);
        assert!(report.checks[0].duration_ms.is_some());
    }
}
