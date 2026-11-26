//! Prometheus 0.0.4 and OpenMetrics 1.0 text exposition renderer.
//!
//! Why: `opentelemetry-prometheus` is deprecated at `0.29` and incompatible with
//! `opentelemetry_sdk = "=0.33.1"`. Rendering directly from [`ResourceMetrics`]
//! returned by [`MetricsRegistry::snapshot`] avoids adding a third-party exporter
//! dependency while supporting both Prometheus 0.0.4 and OpenMetrics 1.0.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use opentelemetry::KeyValue;
use opentelemetry_sdk::metrics::data::{
    AggregatedMetrics, Gauge, Histogram, Metric, MetricData, ResourceMetrics, Sum,
};

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
pub fn render_prometheus(snapshot: &MetricsSnapshot) -> String {
    render_metrics(snapshot, PrometheusFormat::Text004)
}

/// Renders a [`MetricsSnapshot`] in OpenMetrics 1.0.0 text exposition format.
pub fn render_openmetrics(snapshot: &MetricsSnapshot) -> String {
    render_metrics(snapshot, PrometheusFormat::OpenMetrics100)
}

/// Renders a [`MetricsSnapshot`] in the requested [`PrometheusFormat`].
pub fn render_metrics(snapshot: &MetricsSnapshot, format: PrometheusFormat) -> String {
    render_resource_metrics(snapshot.resource_metrics(), format)
}

/// Collects a snapshot from [`MetricsRegistry`] and renders it in the requested format.
pub fn render_registry(registry: &MetricsRegistry, format: PrometheusFormat) -> String {
    render_metrics(&registry.snapshot(), format)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetricType {
    Counter,
    Gauge,
    Histogram,
}

impl MetricType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
            Self::Histogram => "histogram",
        }
    }
}

struct MetricFamily {
    metric_type: MetricType,
    description: String,
    samples: Vec<Sample>,
}

enum Sample {
    Scalar {
        name: String,
        labels: Vec<(String, String)>,
        value: String,
    },
    Histogram {
        name: String,
        labels: Vec<(String, String)>,
        buckets: Vec<(String, u64)>,
        sum: String,
        count: u64,
    },
}

impl Sample {
    fn labels(&self) -> &[(String, String)] {
        match self {
            Self::Scalar { labels, .. } | Self::Histogram { labels, .. } => labels,
        }
    }

    fn sort_tiebreaker(&self) -> &str {
        match self {
            Self::Scalar { value, .. } => value,
            Self::Histogram { sum, .. } => sum,
        }
    }
}

fn render_resource_metrics(
    resource_metrics: &[ResourceMetrics],
    format: PrometheusFormat,
) -> String {
    let mut families: BTreeMap<String, MetricFamily> = BTreeMap::new();

    for rm in resource_metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                collect_metric(&mut families, metric, format);
            }
        }
    }

    let mut out = String::new();
    for (family_name, mut family) in families {
        if family.samples.is_empty() {
            continue;
        }

        family.samples.sort_by(|a, b| {
            a.labels()
                .cmp(b.labels())
                .then_with(|| a.sort_tiebreaker().cmp(b.sort_tiebreaker()))
        });

        if !family.description.is_empty() {
            let escaped = escape_help(&family.description);
            let _ = writeln!(out, "# HELP {family_name} {escaped}");
        }
        let _ = writeln!(out, "# TYPE {family_name} {}", family.metric_type.as_str());

        for sample in &family.samples {
            match sample {
                Sample::Scalar {
                    name,
                    labels,
                    value,
                } => {
                    write_sample_line(&mut out, name, labels, value);
                }
                Sample::Histogram {
                    name,
                    labels,
                    buckets,
                    sum,
                    count,
                } => {
                    for (le, cumulative) in buckets {
                        write_histogram_bucket_line(&mut out, name, labels, le, *cumulative);
                    }
                    let sum_name = format!("{name}_sum");
                    write_sample_line(&mut out, &sum_name, labels, sum);
                    let count_name = format!("{name}_count");
                    write_sample_line(&mut out, &count_name, labels, &count.to_string());
                }
            }
        }
    }

    if format == PrometheusFormat::OpenMetrics100 {
        out.push_str("# EOF\n");
    }

    out
}

fn collect_metric(
    families: &mut BTreeMap<String, MetricFamily>,
    metric: &Metric,
    format: PrometheusFormat,
) {
    let raw_name = sanitize_metric_name(metric.name());
    let description = metric.description();

    match metric.data() {
        AggregatedMetrics::U64(data) => {
            collect_metric_data(families, &raw_name, description, data, format, |v| {
                v.to_string()
            });
        }
        AggregatedMetrics::I64(data) => {
            collect_metric_data(families, &raw_name, description, data, format, |v| {
                v.to_string()
            });
        }
        AggregatedMetrics::F64(data) => {
            collect_metric_data(families, &raw_name, description, data, format, format_f64);
        }
    }
}

fn collect_metric_data<T: Copy>(
    families: &mut BTreeMap<String, MetricFamily>,
    raw_name: &str,
    description: &str,
    data: &MetricData<T>,
    format: PrometheusFormat,
    format_val: fn(T) -> String,
) {
    match data {
        MetricData::Sum(sum) => {
            let metric_type = if sum.is_monotonic() {
                MetricType::Counter
            } else {
                MetricType::Gauge
            };
            let (family_name, sample_name) = resolve_names(raw_name, metric_type, format);
            let family = get_or_insert_family(families, family_name, metric_type, description);
            collect_sum(family, &sample_name, sum, format_val);
        }
        MetricData::Gauge(gauge) => {
            let (family_name, sample_name) = resolve_names(raw_name, MetricType::Gauge, format);
            let family =
                get_or_insert_family(families, family_name, MetricType::Gauge, description);
            collect_gauge(family, &sample_name, gauge, format_val);
        }
        MetricData::Histogram(hist) => {
            let (family_name, sample_name) = resolve_names(raw_name, MetricType::Histogram, format);
            let family =
                get_or_insert_family(families, family_name, MetricType::Histogram, description);
            collect_histogram(family, &sample_name, hist, format_val);
        }
        MetricData::ExponentialHistogram(_) => {
            // Exponential histograms are not emitted by `MetricsRegistry` and have no
            // standard Prometheus 0.0.4 text exposition representation.
        }
    }
}

fn get_or_insert_family<'a>(
    families: &'a mut BTreeMap<String, MetricFamily>,
    family_name: String,
    metric_type: MetricType,
    description: &str,
) -> &'a mut MetricFamily {
    let entry = families.entry(family_name).or_insert_with(|| MetricFamily {
        metric_type,
        description: description.to_string(),
        samples: Vec::new(),
    });
    if entry.description.is_empty() && !description.is_empty() {
        entry.description = description.to_string();
    }
    entry
}

fn resolve_names(
    raw_name: &str,
    metric_type: MetricType,
    format: PrometheusFormat,
) -> (String, String) {
    match (metric_type, format) {
        (MetricType::Counter, PrometheusFormat::OpenMetrics100) => {
            let family = raw_name.strip_suffix("_total").unwrap_or(raw_name);
            let sample = if raw_name.ends_with("_total") {
                raw_name.to_string()
            } else {
                format!("{raw_name}_total")
            };
            (family.to_string(), sample)
        }
        _ => (raw_name.to_string(), raw_name.to_string()),
    }
}

fn collect_sum<T: Copy>(
    family: &mut MetricFamily,
    sample_name: &str,
    sum: &Sum<T>,
    format_val: fn(T) -> String,
) {
    for dp in sum.data_points() {
        family.samples.push(Sample::Scalar {
            name: sample_name.to_string(),
            labels: sorted_labels(dp.attributes()),
            value: format_val(dp.value()),
        });
    }
}

fn collect_gauge<T: Copy>(
    family: &mut MetricFamily,
    sample_name: &str,
    gauge: &Gauge<T>,
    format_val: fn(T) -> String,
) {
    for dp in gauge.data_points() {
        family.samples.push(Sample::Scalar {
            name: sample_name.to_string(),
            labels: sorted_labels(dp.attributes()),
            value: format_val(dp.value()),
        });
    }
}

fn collect_histogram<T: Copy>(
    family: &mut MetricFamily,
    sample_name: &str,
    hist: &Histogram<T>,
    format_val: fn(T) -> String,
) {
    for dp in hist.data_points() {
        let bounds: Vec<f64> = dp.bounds().collect();
        let bucket_counts: Vec<u64> = dp.bucket_counts().collect();

        // Why: OpenTelemetry `ExplicitBucketHistogram` stores per-bucket counts,
        // whereas Prometheus 0.0.4 and OpenMetrics 1.0 require cumulative bucket
        // totals where each `le="<bound>"` includes all earlier buckets and
        // `le="+Inf"` equals `_count`.
        let mut cumulative: u64 = 0;
        let mut buckets = Vec::with_capacity(bounds.len() + 1);
        for (idx, bound) in bounds.iter().enumerate() {
            let count = bucket_counts.get(idx).copied().unwrap_or(0);
            cumulative = cumulative.saturating_add(count);
            buckets.push((format_f64(*bound), cumulative));
        }
        buckets.push(("+Inf".to_string(), dp.count()));

        family.samples.push(Sample::Histogram {
            name: sample_name.to_string(),
            labels: sorted_labels(dp.attributes()),
            buckets,
            sum: format_val(dp.sum()),
            count: dp.count(),
        });
    }
}

fn sorted_labels<'a>(attrs: impl Iterator<Item = &'a KeyValue>) -> Vec<(String, String)> {
    let mut labels: Vec<(String, String)> = attrs
        .map(|kv| {
            (
                sanitize_label_name(kv.key.as_str()),
                kv.value.as_str().into_owned(),
            )
        })
        .collect();
    labels.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    labels
}

fn write_sample_line(out: &mut String, name: &str, labels: &[(String, String)], value: &str) {
    out.push_str(name);
    if !labels.is_empty() {
        out.push('{');
        write_labels(out, labels);
        out.push('}');
    }
    let _ = writeln!(out, " {value}");
}

fn write_histogram_bucket_line(
    out: &mut String,
    name: &str,
    labels: &[(String, String)],
    le: &str,
    cumulative: u64,
) {
    let _ = write!(out, "{name}_bucket{{");
    if !labels.is_empty() {
        write_labels(out, labels);
        out.push(',');
    }
    let _ = writeln!(out, "le=\"{le}\"}} {cumulative}");
}

fn write_labels(out: &mut String, labels: &[(String, String)]) {
    for (idx, (key, val)) in labels.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(key);
        out.push_str("=\"");
        write_escaped_label_value(out, val);
        out.push('"');
    }
}

fn sanitize_metric_name(name: &str) -> String {
    if name.is_empty() {
        return "_".to_string();
    }
    let mut out = String::with_capacity(name.len());
    for (idx, ch) in name.chars().enumerate() {
        if idx == 0 && ch.is_ascii_digit() {
            out.push('_');
        }
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == ':' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

fn sanitize_label_name(name: &str) -> String {
    if name.is_empty() {
        return "_".to_string();
    }
    let mut out = String::with_capacity(name.len());
    for (idx, ch) in name.chars().enumerate() {
        if idx == 0 && ch.is_ascii_digit() {
            out.push('_');
        }
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

fn escape_help(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out
}

fn write_escaped_label_value(out: &mut String, val: &str) {
    for ch in val.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
}

fn format_f64(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value.is_infinite() {
        if value.is_sign_positive() {
            "+Inf".to_string()
        } else {
            "-Inf".to_string()
        }
    } else if value == 0.0 {
        "0".to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Weak};
    use std::time::Duration;

    use opentelemetry::KeyValue;
    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use opentelemetry_sdk::metrics::reader::MetricReader;
    use opentelemetry_sdk::metrics::{
        InstrumentKind, ManualReader, Pipeline, SdkMeterProvider, Temporality,
    };
    use rume_foundation::health::HealthStatus;
    use rume_foundation::telemetry::PoolStatus;

    use super::*;

    #[derive(Debug)]
    struct TestManualReader(Arc<ManualReader>);

    impl MetricReader for TestManualReader {
        fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
            self.0.register_pipeline(pipeline);
        }

        fn collect(&self, rm: &mut ResourceMetrics) -> OTelSdkResult {
            self.0.collect(rm)
        }

        fn force_flush(&self) -> OTelSdkResult {
            self.0.force_flush()
        }

        fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
            self.0.shutdown_with_timeout(timeout)
        }

        fn temporality(&self, kind: InstrumentKind) -> Temporality {
            self.0.temporality(kind)
        }
    }

    #[test]
    fn prometheus_renders_counters_gauges_and_cumulative_histograms() {
        let registry = MetricsRegistry::new();
        registry.record_http_request("GET", 200, Duration::from_millis(12));
        registry.record_http_request("GET", 200, Duration::from_millis(300));
        registry.inc_in_flight();
        registry.record_db_pool_status(
            "primary",
            &PoolStatus {
                leased_connections: 2,
                active_connections: 3,
                idle_connections: 7,
                total_connections: 10,
                max_connections: 20,
                in_flight_transactions: 1,
                is_closed: false,
            },
        );
        registry.record_health_check("database", HealthStatus::Ok, Duration::from_millis(5));

        let output = render_prometheus(&registry.snapshot());

        // Counter metadata and sample
        assert!(
            output.contains("# HELP http_requests_total "),
            "missing HELP for http_requests_total:\n{output}"
        );
        assert!(
            output.contains("# TYPE http_requests_total counter\n"),
            "missing TYPE for http_requests_total:\n{output}"
        );
        assert!(
            output.contains("http_requests_total{method=\"GET\",status=\"200\"} 2\n"),
            "missing counter sample:\n{output}"
        );

        // Gauge metadata and samples
        assert!(
            output.contains("# TYPE http_requests_in_flight gauge\n"),
            "missing TYPE for http_requests_in_flight:\n{output}"
        );
        assert!(
            output.contains("http_requests_in_flight 1\n"),
            "missing in-flight gauge sample:\n{output}"
        );
        assert!(
            output.contains("# TYPE db_pool_connections gauge\n"),
            "missing TYPE for db_pool_connections:\n{output}"
        );
        assert!(
            output.contains("db_pool_connections{pool=\"primary\",state=\"active\"} 3\n"),
            "missing db_pool_connections active sample:\n{output}"
        );
        assert!(
            output.contains("health_check_status{name=\"database\"} 1\n"),
            "missing health_check_status sample:\n{output}"
        );

        // Cumulative histogram buckets: 12ms (0.012s) in <=0.025, 300ms (0.3s) in <=0.5
        assert!(
            output.contains("# TYPE http_request_duration_seconds histogram\n"),
            "missing TYPE for histogram:\n{output}"
        );
        assert!(
            output.contains("http_request_duration_seconds_bucket{method=\"GET\",le=\"0.01\"} 0\n"),
            "expected 0 in le=0.01:\n{output}"
        );
        assert!(
            output
                .contains("http_request_duration_seconds_bucket{method=\"GET\",le=\"0.025\"} 1\n"),
            "expected 1 in le=0.025:\n{output}"
        );
        assert!(
            output.contains("http_request_duration_seconds_bucket{method=\"GET\",le=\"0.25\"} 1\n"),
            "expected cumulative 1 in le=0.25:\n{output}"
        );
        assert!(
            output.contains("http_request_duration_seconds_bucket{method=\"GET\",le=\"0.5\"} 2\n"),
            "expected cumulative 2 in le=0.5 (includes earlier 0.012s bucket):\n{output}"
        );
        assert!(
            output.contains("http_request_duration_seconds_bucket{method=\"GET\",le=\"+Inf\"} 2\n"),
            "expected le=+Inf to equal count (2):\n{output}"
        );
        assert!(
            output.contains("http_request_duration_seconds_count{method=\"GET\"} 2\n"),
            "expected _count to be 2:\n{output}"
        );
        assert!(
            output.contains("http_request_duration_seconds_sum{method=\"GET\"} 0.312\n"),
            "expected _sum to be 0.312:\n{output}"
        );
    }

    #[test]
    fn prometheus_escapes_label_values_and_help_text() {
        let registry = MetricsRegistry::new();
        registry.record_health_check(
            "db\\primary\"line1\nline2",
            HealthStatus::Ok,
            Duration::from_millis(1),
        );

        let output = render_prometheus(&registry.snapshot());
        assert!(
            output.contains("health_check_status{name=\"db\\\\primary\\\"line1\\nline2\"} 1\n"),
            "label value not properly escaped:\n{output}"
        );

        // Verify # HELP escaping with a real OTel instrument whose description has \ and \n.
        let reader = Arc::new(ManualReader::builder().build());
        let provider = SdkMeterProvider::builder()
            .with_reader(TestManualReader(Arc::clone(&reader)))
            .build();
        let meter = provider.meter("test");
        let counter = meter
            .u64_counter("custom.escaped-total")
            .with_description("Path C:\\metrics\nSecond line")
            .build();
        counter.add(1, &[KeyValue::new("k", "a\\b\"c\nd")]);

        let mut rm = ResourceMetrics::default();
        reader.collect(&mut rm).expect("collect metrics");
        let rendered = render_resource_metrics(&[rm], PrometheusFormat::Text004);
        assert!(
            rendered.contains("# HELP custom_escaped_total Path C:\\\\metrics\\nSecond line\n"),
            "HELP description not properly escaped:\n{rendered}"
        );
        assert!(
            rendered.contains("custom_escaped_total{k=\"a\\\\b\\\"c\\nd\"} 1\n"),
            "custom label value not properly escaped:\n{rendered}"
        );
        let _ = provider.shutdown();
    }

    #[test]
    fn openmetrics_appends_eof_sentinel_and_uses_counter_family_name() {
        let registry = MetricsRegistry::new();
        registry.record_http_request("GET", 200, Duration::from_millis(10));
        let snap = registry.snapshot();

        let om = render_openmetrics(&snap);
        assert!(
            om.ends_with("# EOF\n"),
            "OpenMetrics must end with # EOF\\n:\n{om}"
        );
        assert!(
            om.contains("# HELP http_requests "),
            "OpenMetrics counter HELP must strip _total:\n{om}"
        );
        assert!(
            om.contains("# TYPE http_requests counter\n"),
            "OpenMetrics counter TYPE must strip _total:\n{om}"
        );
        assert!(
            om.contains("http_requests_total{method=\"GET\",status=\"200\"} 1\n"),
            "OpenMetrics counter sample must carry _total:\n{om}"
        );

        let prom = render_prometheus(&snap);
        assert!(
            !prom.ends_with("# EOF\n"),
            "Prometheus 0.0.4 must not end with # EOF\\n:\n{prom}"
        );
        assert!(
            prom.contains("# TYPE http_requests_total counter\n"),
            "Prometheus 0.0.4 must keep _total in TYPE:\n{prom}"
        );
    }

    #[test]
    fn prometheus_output_is_deterministic_across_calls() {
        let registry = MetricsRegistry::new();
        registry.record_http_request("GET", 200, Duration::from_millis(12));
        registry.record_http_request("POST", 201, Duration::from_millis(55));
        registry.record_health_check("db", HealthStatus::Ok, Duration::from_millis(3));
        registry.record_health_check("cache", HealthStatus::Failed, Duration::from_millis(7));

        let snap = registry.snapshot();
        let first = render_prometheus(&snap);
        let second = render_prometheus(&snap);
        assert_eq!(first, second);
    }
}
