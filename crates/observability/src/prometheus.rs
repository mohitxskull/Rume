//! Prometheus 0.0.4 and OpenMetrics 1.0 text exposition renderer.

use crate::metrics::{MetricsRegistry, MetricsSnapshot};

/// HTTP `Content-Type` header value for Prometheus text exposition format 0.0.4.
pub const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// HTTP `Content-Type` header value for OpenMetrics text exposition format 1.0.0.
pub const OPENMETRICS_CONTENT_TYPE: &str =
    "application/openmetrics-text; version=1.0.0; charset=utf-8";

/// Text exposition format for metrics scraping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrometheusFormat {
    /// Prometheus text exposition format 0.0.4 (default).
    #[default]
    Text004,
    /// OpenMetrics 1.0.0 text exposition format (ends with `# EOF\n`).
    OpenMetrics100,
}

/// Renders a [`MetricsSnapshot`] in Prometheus 0.0.4 text exposition format.
#[allow(unused_variables, dead_code)] // Removed in Task 03.
pub fn render_prometheus(snapshot: &MetricsSnapshot) -> String {
    todo!("implemented in Task 03")
}

/// Renders a [`MetricsSnapshot`] in OpenMetrics 1.0.0 text exposition format.
#[allow(unused_variables, dead_code)] // Removed in Task 03.
pub fn render_openmetrics(snapshot: &MetricsSnapshot) -> String {
    todo!("implemented in Task 03")
}

/// Renders a [`MetricsSnapshot`] in the requested [`PrometheusFormat`].
#[allow(unused_variables, dead_code)] // Removed in Task 03.
pub fn render_metrics(snapshot: &MetricsSnapshot, format: PrometheusFormat) -> String {
    todo!("implemented in Task 03")
}

/// Collects a snapshot from [`MetricsRegistry`] and renders it in the requested format.
#[allow(unused_variables, dead_code)] // Removed in Task 03.
pub fn render_registry(registry: &MetricsRegistry, format: PrometheusFormat) -> String {
    todo!("implemented in Task 03")
}
