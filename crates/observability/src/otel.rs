//! OpenTelemetry subscriber initialization (`SdkTracerProvider`, `SdkMeterProvider`, `SdkLoggerProvider`)
//! and optional OTLP export (`feature = "otel"`).

use std::sync::atomic::{AtomicBool, Ordering};

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Guard that shuts down OpenTelemetry providers when dropped.
#[derive(Debug)]
pub struct ObservabilityGuard {
    tracer_provider: SdkTracerProvider,
    meter_provider: SdkMeterProvider,
    logger_provider: SdkLoggerProvider,
    shut_down: AtomicBool,
}

impl ObservabilityGuard {
    /// Returns a reference to the underlying [`SdkTracerProvider`].
    pub fn tracer_provider(&self) -> &SdkTracerProvider {
        &self.tracer_provider
    }

    /// Returns a reference to the underlying [`SdkMeterProvider`].
    pub fn meter_provider(&self) -> &SdkMeterProvider {
        &self.meter_provider
    }

    /// Returns a reference to the underlying [`SdkLoggerProvider`].
    pub fn logger_provider(&self) -> &SdkLoggerProvider {
        &self.logger_provider
    }

    /// Explicitly shuts down all OpenTelemetry providers synchronously.
    pub fn shutdown(&self) {
        if self.shut_down.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.tracer_provider.shutdown();
        let _ = self.meter_provider.shutdown();
        let _ = self.logger_provider.shutdown();
    }

    /// Explicitly shuts down all OpenTelemetry providers without blocking the
    /// calling async runtime thread.
    pub async fn shutdown_async(&self) {
        if self.shut_down.swap(true, Ordering::AcqRel) {
            return;
        }
        let tracer = self.tracer_provider.clone();
        let meter = self.meter_provider.clone();
        let logger = self.logger_provider.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = tracer.shutdown();
            let _ = meter.shutdown();
            let _ = logger.shutdown();
        })
        .await;
    }
}

impl Drop for ObservabilityGuard {
    fn drop(&mut self) {
        if self.shut_down.swap(true, Ordering::AcqRel) {
            return;
        }
        // Why: `Sdk*Provider::shutdown()` performs blocking flushes (and HTTP
        // OTLP exports when `feature = "otel"` is enabled).
        // - Outside Tokio: run synchronously.
        // - Inside a multi-threaded Tokio runtime: use `block_in_place` so
        //   shutdown completes before process exit without starving worker threads.
        // - Inside a `current_thread` Tokio runtime: `block_in_place` panics and
        //   blocking directly can deadlock, so offload to an OS thread; async
        //   callers on `current_thread` runtimes should await `shutdown_async()`
        //   before exiting to guarantee all pending telemetry flushes.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
                tokio::task::block_in_place(|| {
                    let _ = self.tracer_provider.shutdown();
                    let _ = self.meter_provider.shutdown();
                    let _ = self.logger_provider.shutdown();
                });
                return;
            }
            let tracer = self.tracer_provider.clone();
            let meter = self.meter_provider.clone();
            let logger = self.logger_provider.clone();
            std::thread::spawn(move || {
                let _ = tracer.shutdown();
                let _ = meter.shutdown();
                let _ = logger.shutdown();
            });
            return;
        }
        let _ = self.tracer_provider.shutdown();
        let _ = self.meter_provider.shutdown();
        let _ = self.logger_provider.shutdown();
    }
}

/// Builder for configuring and installing Rume's unified OpenTelemetry + `tracing` subscriber.
#[derive(Debug, Clone)]
pub struct ObservabilityBuilder {
    service_name: String,
    #[cfg(feature = "otel")]
    otlp_endpoint: Option<String>,
}

impl Default for ObservabilityBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ObservabilityBuilder {
    /// Creates a new observability builder with default service name `"rume"`.
    pub fn new() -> Self {
        Self {
            service_name: "rume".to_string(),
            #[cfg(feature = "otel")]
            otlp_endpoint: std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        }
    }

    /// Sets the OpenTelemetry `service.name` resource attribute.
    pub fn with_service_name(mut self, service_name: impl Into<String>) -> Self {
        self.service_name = service_name.into();
        self
    }

    /// Configures an explicit OTLP HTTP endpoint for traces, metrics, and logs.
    #[cfg(feature = "otel")]
    pub fn with_otlp_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        let endpoint = endpoint.into();
        self.otlp_endpoint = if endpoint.trim().is_empty() {
            None
        } else {
            Some(endpoint)
        };
        self
    }

    /// Installs the global `TraceContextPropagator`, OpenTelemetry providers, and `tracing`
    /// subscriber (`fmt` + `OpenTelemetryLayer` + `OpenTelemetryTracingBridge`).
    ///
    /// Returns an [`Err`] without panicking if a global `tracing` subscriber is already installed.
    pub fn try_init(self) -> Result<ObservabilityGuard, Box<dyn std::error::Error + Send + Sync>> {
        let resource = Resource::builder()
            .with_service_name(self.service_name.clone())
            .build();

        let (tracer_provider, meter_provider, logger_provider) = self.build_providers(resource)?;

        let tracer = tracer_provider.tracer("rume");
        let otel_trace_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        let otel_log_layer = OpenTelemetryTracingBridge::new(&logger_provider);
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

        // Why: install the global `tracing` subscriber BEFORE mutating global
        // OpenTelemetry propagators/providers. If a subscriber is already
        // registered, mutating globals first would clobber custom propagators
        // or providers and violate the "safely ignored" contract of `init_tracing`.
        if let Err(err) = tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .with(otel_trace_layer)
            .with(otel_log_layer)
            .try_init()
        {
            if tokio::runtime::Handle::try_current().is_ok() {
                std::thread::spawn(move || {
                    let _ = tracer_provider.shutdown();
                    let _ = meter_provider.shutdown();
                    let _ = logger_provider.shutdown();
                });
            } else {
                let _ = tracer_provider.shutdown();
                let _ = meter_provider.shutdown();
                let _ = logger_provider.shutdown();
            }
            return Err(err.into());
        }

        global::set_text_map_propagator(TraceContextPropagator::new());
        global::set_tracer_provider(tracer_provider.clone());
        global::set_meter_provider(meter_provider.clone());

        Ok(ObservabilityGuard {
            tracer_provider,
            meter_provider,
            logger_provider,
            shut_down: AtomicBool::new(false),
        })
    }

    #[cfg(not(feature = "otel"))]
    fn build_providers(
        &self,
        resource: Resource,
    ) -> Result<
        (SdkTracerProvider, SdkMeterProvider, SdkLoggerProvider),
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let tracer_provider = SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .build();
        let meter_provider = SdkMeterProvider::builder()
            .with_resource(resource.clone())
            .build();
        let logger_provider = SdkLoggerProvider::builder().with_resource(resource).build();
        Ok((tracer_provider, meter_provider, logger_provider))
    }

    #[cfg(feature = "otel")]
    fn build_providers(
        &self,
        resource: Resource,
    ) -> Result<
        (SdkTracerProvider, SdkMeterProvider, SdkLoggerProvider),
        Box<dyn std::error::Error + Send + Sync>,
    > {
        use opentelemetry_otlp::{Protocol, WithExportConfig};

        let Some(endpoint) = self
            .otlp_endpoint
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            let tracer_provider = SdkTracerProvider::builder()
                .with_resource(resource.clone())
                .build();
            let meter_provider = SdkMeterProvider::builder()
                .with_resource(resource.clone())
                .build();
            let logger_provider = SdkLoggerProvider::builder().with_resource(resource).build();
            return Ok((tracer_provider, meter_provider, logger_provider));
        };

        let trace_endpoint = normalize_otlp_http_endpoint(endpoint, "/v1/traces");
        let span_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(trace_endpoint)
            .build()?;
        let tracer_provider = SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(span_exporter)
            .build();

        let metric_endpoint = normalize_otlp_http_endpoint(endpoint, "/v1/metrics");
        let metric_exporter = match opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(metric_endpoint)
            .build()
        {
            Ok(exp) => exp,
            Err(err) => {
                // Why: shut down already-built `tracer_provider` on partial init failure
                // so its background batch exporter task does not leak (`RC-25`).
                let _ = tracer_provider.shutdown();
                return Err(err.into());
            }
        };
        let meter_provider = SdkMeterProvider::builder()
            .with_resource(resource.clone())
            .with_periodic_exporter(metric_exporter)
            .build();

        let log_endpoint = normalize_otlp_http_endpoint(endpoint, "/v1/logs");
        let log_exporter = match opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(log_endpoint)
            .build()
        {
            Ok(exp) => exp,
            Err(err) => {
                let _ = tracer_provider.shutdown();
                let _ = meter_provider.shutdown();
                return Err(err.into());
            }
        };
        let logger_provider = SdkLoggerProvider::builder()
            .with_resource(resource)
            .with_batch_exporter(log_exporter)
            .build();

        Ok((tracer_provider, meter_provider, logger_provider))
    }
}

/// Normalizes an OTLP HTTP base endpoint URL by appending the per-signal path
/// (`/v1/traces`, `/v1/metrics`, or `/v1/logs`) when not already present (`RC-25`).
///
/// Why: `opentelemetry-otlp`'s `.with_endpoint(...)` treats the argument as the full
/// per-signal URL rather than a collector base URL; passing `http://collector:4318`
/// directly would POST all three signals to `/` instead of `/v1/{traces,metrics,logs}`.
#[cfg(feature = "otel")]
pub(crate) fn normalize_otlp_http_endpoint(base: &str, signal_path: &str) -> String {
    let trimmed = base.trim().trim_end_matches('/');
    let suffix = signal_path.trim_start_matches('/');
    let full_suffix = format!("/{suffix}");
    if trimmed.ends_with(&full_suffix) {
        trimmed.to_string()
    } else {
        format!("{trimmed}{full_suffix}")
    }
}

static GLOBAL_OBSERVABILITY_GUARD: std::sync::OnceLock<ObservabilityGuard> =
    std::sync::OnceLock::new();

/// Flushes and shuts down the global observability providers installed by [`try_init_tracing`].
///
/// Safe to call multiple times or when [`try_init_tracing`] was never called.
pub fn shutdown_global_observability() {
    if let Some(guard) = GLOBAL_OBSERVABILITY_GUARD.get() {
        guard.shutdown();
    }
}

/// Non-panicking initializer for the default global tracing subscriber with OpenTelemetry
/// trace/log bridging and W3C `TraceContextPropagator`.
///
/// Returns `Ok(())` on successful initialization, or `Err(err)` if a global subscriber
/// has already been set.
pub fn try_init_tracing() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let guard = ObservabilityBuilder::new().try_init()?;
    let _ = GLOBAL_OBSERVABILITY_GUARD.set(guard);
    Ok(())
}

/// Initialize the tracing subscriber if not already initialized.
///
/// Reads `RUST_LOG` (or defaults to `info`) to set the filtering level, then
/// installs the OpenTelemetry-bridged subscriber. If a global subscriber has already
/// been registered (e.g. during tests or multiple reboots), this is safely ignored.
pub fn init_tracing() {
    let _ = try_init_tracing();
}

#[cfg(test)]
pub(crate) fn test_global_otel_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observability_try_init_tracing_is_idempotent_and_non_panicking() {
        let _global_guard = test_global_otel_lock().blocking_lock();
        init_tracing();
        init_tracing();
        let second_try = try_init_tracing();
        assert!(second_try.is_err());
    }

    #[test]
    fn failed_reinit_preserves_custom_global_propagator() {
        use opentelemetry_sdk::propagation::BaggagePropagator;

        let _global_guard = test_global_otel_lock().blocking_lock();
        init_tracing();

        // Install a custom propagator (BaggagePropagator injects `baggage`, not `traceparent`).
        global::set_text_map_propagator(BaggagePropagator::new());

        // Calling init_tracing() again must fail to install a subscriber and must NOT overwrite
        // the global propagator back to TraceContextPropagator.
        let res = ObservabilityBuilder::new().try_init();
        assert!(res.is_err());

        let fields: Vec<String> =
            global::get_text_map_propagator(|p| p.fields().map(str::to_string).collect());
        assert!(
            fields.iter().any(|f| f == "baggage"),
            "custom BaggagePropagator should survive failed try_init, got fields: {fields:?}"
        );

        // Restore TraceContextPropagator for subsequent tests.
        global::set_text_map_propagator(TraceContextPropagator::new());
    }

    #[tokio::test]
    async fn dropping_observability_guard_in_current_thread_runtime_does_not_deadlock() {
        let resource = Resource::builder().with_service_name("test").build();
        let builder = ObservabilityBuilder::new();
        let (tracer_provider, meter_provider, logger_provider) =
            builder.build_providers(resource).expect("build providers");
        let guard = ObservabilityGuard {
            tracer_provider,
            meter_provider,
            logger_provider,
            shut_down: AtomicBool::new(false),
        };
        drop(guard);
    }

    #[cfg(feature = "otel")]
    #[test]
    fn whitespace_otlp_endpoint_is_treated_as_none() {
        let builder = ObservabilityBuilder::new().with_otlp_endpoint("   ");
        assert!(builder.otlp_endpoint.is_none());
        let resource = Resource::builder().with_service_name("test").build();
        let (t, m, l) = builder.build_providers(resource).expect("build providers");
        let _ = t.shutdown();
        let _ = m.shutdown();
        let _ = l.shutdown();
    }

    #[cfg(feature = "otel")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn otlp_http_base_endpoint_posts_to_per_signal_paths() {
        use opentelemetry::logs::{LogRecord as _, Logger as _, LoggerProvider as _};
        use opentelemetry::metrics::MeterProvider as _;
        use opentelemetry::trace::Tracer as _;
        use std::collections::HashSet;
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        assert_eq!(
            normalize_otlp_http_endpoint("http://127.0.0.1:4318/", "/v1/traces"),
            "http://127.0.0.1:4318/v1/traces"
        );
        assert_eq!(
            normalize_otlp_http_endpoint("http://127.0.0.1:4318/v1/traces", "/v1/traces"),
            "http://127.0.0.1:4318/v1/traces"
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let seen_paths: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let seen_clone = Arc::clone(&seen_paths);

        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let seen = Arc::clone(&seen_clone);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    if let Ok(n) = stream.read(&mut buf).await
                        && n > 0
                    {
                        let req = String::from_utf8_lossy(&buf[..n]);
                        if let Some(first_line) = req.lines().next()
                            && let Some(path) = first_line.split_whitespace().nth(1)
                        {
                            seen.lock().unwrap().insert(path.to_string());
                        }
                    }
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                    let _ = stream.shutdown().await;
                });
            }
        });

        let resource = Resource::builder().with_service_name("otel-test").build();
        let builder = ObservabilityBuilder::new().with_otlp_endpoint(format!("http://{addr}"));
        let (tracer_provider, meter_provider, logger_provider) =
            builder.build_providers(resource).expect("build providers");

        // Emit one span, one metric observation, and one log record, then flush/shutdown.
        tracer_provider.tracer("test").in_span("ping", |_| {});
        let counter = meter_provider.meter("test").u64_counter("reqs").build();
        counter.add(1, &[]);
        let logger = logger_provider.logger("test");
        let mut record = logger.create_log_record();
        record.set_body("hello".into());
        logger.emit(record);

        tokio::task::spawn_blocking(move || {
            let _ = tracer_provider.force_flush();
            let _ = meter_provider.force_flush();
            let _ = logger_provider.force_flush();
            let _ = tracer_provider.shutdown();
            let _ = meter_provider.shutdown();
            let _ = logger_provider.shutdown();
        })
        .await
        .expect("flush providers");

        server.abort();
        let paths = seen_paths.lock().unwrap().clone();
        assert!(
            paths.contains("/v1/traces"),
            "expected POST to /v1/traces, saw {paths:?}"
        );
        assert!(
            paths.contains("/v1/metrics"),
            "expected POST to /v1/metrics, saw {paths:?}"
        );
        assert!(
            paths.contains("/v1/logs"),
            "expected POST to /v1/logs, saw {paths:?}"
        );
    }
}
