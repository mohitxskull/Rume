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
        // OTLP exports when `feature = "otel"` is enabled). Calling it directly
        // on a Tokio worker thread can deadlock a `current_thread` runtime;
        // offload to a dedicated OS thread when inside a Tokio runtime, matching
        // `MetricsRegistry::Inner::drop`.
        if tokio::runtime::Handle::try_current().is_ok() {
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

        let span_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(endpoint)
            .build()?;
        let tracer_provider = SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(span_exporter)
            .build();

        let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(endpoint)
            .build()?;
        let meter_provider = SdkMeterProvider::builder()
            .with_resource(resource.clone())
            .with_periodic_exporter(metric_exporter)
            .build();

        let log_exporter = opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(endpoint)
            .build()?;
        let logger_provider = SdkLoggerProvider::builder()
            .with_resource(resource)
            .with_batch_exporter(log_exporter)
            .build();

        Ok((tracer_provider, meter_provider, logger_provider))
    }
}

/// Non-panicking initializer for the default global tracing subscriber with OpenTelemetry
/// trace/log bridging and W3C `TraceContextPropagator`.
///
/// Returns `Ok(())` on successful initialization, or `Err(err)` if a global subscriber
/// has already been set.
pub fn try_init_tracing() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let guard = ObservabilityBuilder::new().try_init()?;
    // Why: `try_init_tracing` has no guard return type for backwards compatibility;
    // keeping providers alive for the process lifetime prevents premature shutdown
    // of the global tracer/meter/logger providers.
    std::mem::forget(guard);
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
}
