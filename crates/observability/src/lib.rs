//! Unified OpenTelemetry observability layer (traces, logs, W3C context, and metrics).

#![forbid(unsafe_code)]

pub mod layer;
pub mod metrics;
pub mod otel;
pub mod trace_context;

pub use layer::{ObservabilityLayer, ObservabilityService};
pub use metrics::{
    Counter, DynamicPoolExporter, Gauge, MetricsLayer, MetricsRegistry, MetricsService,
    MetricsSnapshot, PoolMetricsExporter,
};
pub use otel::{ObservabilityBuilder, ObservabilityGuard, init_tracing, try_init_tracing};
pub use rume_foundation::telemetry::{PoolStatus, RequestId, X_REQUEST_ID, is_valid_request_id};
pub use trace_context::{TRACEPARENT, TRACESTATE, TraceContext};
