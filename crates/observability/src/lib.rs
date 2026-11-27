//! Unified OpenTelemetry observability layer (traces, logs, W3C context, and metrics).

#![forbid(unsafe_code)]

pub mod layer;
pub mod metrics;
pub mod otel;
pub mod prometheus;
pub mod redaction;
pub mod trace_context;

pub use layer::{ObservabilityLayer, ObservabilityService};
pub use metrics::{
    DynamicPoolExporter, MetricsLayer, MetricsRegistry, MetricsService, MetricsSnapshot,
    PoolMetricsExporter,
};
pub use otel::{
    ObservabilityBuilder, ObservabilityGuard, init_tracing, shutdown_global_observability,
    try_init_tracing,
};
pub use prometheus::{
    OPENMETRICS_CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE, PrometheusFormat, render_metrics,
    render_openmetrics, render_prometheus, render_registry,
};
pub use redaction::{
    DEFAULT_REDACTED_PLACEHOLDER, DEFAULT_SENSITIVE_HEADERS, DEFAULT_SENSITIVE_JSON_KEYS,
    RedactionConfig,
};
pub use rume_foundation::telemetry::{PoolStatus, RequestId, X_REQUEST_ID, is_valid_request_id};
pub use trace_context::{TRACEPARENT, TRACESTATE, TraceContext};
