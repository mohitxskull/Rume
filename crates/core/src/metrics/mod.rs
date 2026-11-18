//! Prometheus metrics collection, registry, and Tower middleware (re-exported from `rume-observability`).

pub use rume_observability::metrics::{
    Counter, DynamicPoolExporter, Gauge, MetricsLayer, MetricsRegistry, MetricsService,
    PoolMetricsExporter,
};
