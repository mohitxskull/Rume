//! W3C TraceContext representation and OpenTelemetry context bridging.

use std::fmt::{self, Display};

use axum::extract::FromRequestParts;
use axum::http::header::HeaderName;
use axum::http::request::Parts;
use opentelemetry::Context as OtelContext;
use opentelemetry::propagation::{Extractor, Injector, TextMapPropagator};
use opentelemetry::trace::TraceContextExt;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use serde::{Deserialize, Serialize};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Standard W3C TraceContext header name: `traceparent`.
pub static TRACEPARENT: HeaderName = HeaderName::from_static("traceparent");

/// Standard W3C TraceContext header name: `tracestate`.
pub static TRACESTATE: HeaderName = HeaderName::from_static("tracestate");

struct TraceContextInjector {
    traceparent: Option<String>,
    tracestate: Option<String>,
}

impl TraceContextInjector {
    fn new() -> Self {
        Self {
            traceparent: None,
            tracestate: None,
        }
    }
}

impl Injector for TraceContextInjector {
    fn set(&mut self, key: &str, value: String) {
        if key.eq_ignore_ascii_case("traceparent") {
            self.traceparent = Some(value);
        } else if key.eq_ignore_ascii_case("tracestate") && !value.is_empty() {
            self.tracestate = Some(value);
        }
    }
}

/// W3C TraceContext representation propagating distributed trace identity across HTTP, jobs, and outbox.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TraceContext {
    /// W3C `traceparent` header string (e.g. `00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01`).
    pub traceparent: String,
    /// Optional W3C `tracestate` header string.
    pub tracestate: Option<String>,
}

impl Extractor for TraceContext {
    fn get(&self, key: &str) -> Option<&str> {
        if key.eq_ignore_ascii_case("traceparent") {
            Some(self.traceparent.as_str())
        } else if key.eq_ignore_ascii_case("tracestate") {
            self.tracestate.as_deref()
        } else {
            None
        }
    }

    fn keys(&self) -> Vec<&str> {
        if self.tracestate.is_some() {
            vec!["traceparent", "tracestate"]
        } else {
            vec!["traceparent"]
        }
    }
}

impl TraceContext {
    /// Returns the currently active trace context from the active `tracing::Span`
    /// or `opentelemetry::Context`, if a valid span context is present.
    pub fn current() -> Option<Self> {
        let span_cx = tracing::Span::current().context();
        if let Some(tc) = Self::from_otel_context(&span_cx) {
            return Some(tc);
        }

        // Why: when no OpenTelemetry tracing layer is installed on the global
        // subscriber (e.g. unit tests or minimal setups), `Span::context()`
        // returns an empty context. Falling back to `opentelemetry::Context::current()`
        // preserves W3C propagation attached via `FutureExt::with_context`.
        let otel_cx = OtelContext::current();
        Self::from_otel_context(&otel_cx)
    }

    /// Converts this `TraceContext` into an [`opentelemetry::Context`] using W3C `TraceContextPropagator`.
    pub fn to_otel_context(&self) -> OtelContext {
        TraceContextPropagator::new().extract(self)
    }

    /// Extracts a `TraceContext` from an [`opentelemetry::Context`] if it carries a valid span context.
    pub fn from_otel_context(cx: &OtelContext) -> Option<Self> {
        if !cx.span().span_context().is_valid() {
            return None;
        }
        let mut injector = TraceContextInjector::new();
        TraceContextPropagator::new().inject_context(cx, &mut injector);
        injector.traceparent.map(|traceparent| Self {
            traceparent,
            tracestate: injector.tracestate,
        })
    }

    /// Attaches this `TraceContext` as the OpenTelemetry parent of `span` and returns the
    /// resulting [`opentelemetry::Context`].
    pub fn attach_to_span(&self, span: &tracing::Span) -> OtelContext {
        let cx = self.to_otel_context();
        let _ = span.set_parent(cx.clone());
        cx
    }

    /// Attaches this `TraceContext` to `span` and wraps `fut` with the resulting
    /// OpenTelemetry [`opentelemetry::Context`] and `tracing::Instrument`.
    pub fn scope_future<F>(
        &self,
        span: tracing::Span,
        fut: F,
    ) -> impl std::future::Future<Output = F::Output> + use<F>
    where
        F: std::future::Future,
    {
        use opentelemetry::context::FutureExt as _;
        use tracing::Instrument as _;
        let cx = self.attach_to_span(&span);
        fut.with_context(cx).instrument(span)
    }

    /// Extracts the 32-character hex trace ID from the `traceparent`.
    pub fn trace_id(&self) -> &str {
        self.traceparent.split('-').nth(1).unwrap_or("")
    }

    /// Extracts the 16-character hex span ID from the `traceparent`.
    pub fn span_id(&self) -> &str {
        self.traceparent.split('-').nth(2).unwrap_or("")
    }

    /// Extracts the 2-character hex flags from the `traceparent`.
    pub fn flags(&self) -> &str {
        self.traceparent.split('-').nth(3).unwrap_or("01")
    }

    /// Creates a child `TraceContext` that preserves the trace ID and flags, but generates a new random span ID.
    pub fn new_child(&self) -> Self {
        let trace_id = self.trace_id();
        let flags = self.flags();
        let new_span_id = format!("{:016x}", uuid::Uuid::new_v4().as_u128() as u64);
        Self {
            traceparent: format!("00-{trace_id}-{new_span_id}-{flags}"),
            tracestate: self.tracestate.clone(),
        }
    }

    /// Generates a new valid W3C TraceContext traceparent.
    pub fn generate() -> Self {
        let trace_id = format!("{:032x}", uuid::Uuid::new_v4().as_u128());
        let span_id = format!("{:016x}", uuid::Uuid::new_v4().as_u128() as u64);
        Self {
            traceparent: format!("00-{trace_id}-{span_id}-01"),
            tracestate: None,
        }
    }

    /// Creates a `TraceContext` from an explicit `traceparent` string.
    pub fn from_traceparent(traceparent: impl Into<String>) -> Self {
        Self {
            traceparent: traceparent.into(),
            tracestate: None,
        }
    }

    /// Validates whether a string matches W3C `traceparent` specification.
    pub fn is_valid_traceparent(s: &str) -> bool {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 4 {
            return false;
        }
        if parts[0] != "00" {
            return false;
        }
        if parts[1].len() != 32
            || !parts[1].chars().all(|c| c.is_ascii_hexdigit())
            || parts[1] == "00000000000000000000000000000000"
        {
            return false;
        }
        if parts[2].len() != 16
            || !parts[2].chars().all(|c| c.is_ascii_hexdigit())
            || parts[2] == "0000000000000000"
        {
            return false;
        }
        if parts[3].len() != 2 || !parts[3].chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
        true
    }

    /// Serializes the trace context into a persistent string preserving both `traceparent` and `tracestate`.
    pub fn to_serialized(&self) -> String {
        if let Some(ref ts) = self.tracestate {
            format!("{}\n{}", self.traceparent, ts)
        } else {
            self.traceparent.clone()
        }
    }

    /// Parses a serialized trace context string, reconstructing both `traceparent` and `tracestate`.
    pub fn parse_serialized(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some((tp, ts)) = s.split_once('\n') {
            let tp = tp.trim();
            let ts = ts.trim();
            if Self::is_valid_traceparent(tp) {
                return Some(Self {
                    traceparent: tp.to_string(),
                    tracestate: if ts.is_empty() {
                        None
                    } else {
                        Some(ts.to_string())
                    },
                });
            }
        }
        if Self::is_valid_traceparent(s) {
            Some(Self::from_traceparent(s))
        } else {
            None
        }
    }
}

impl Display for TraceContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_serialized())
    }
}

impl<S> FromRequestParts<S> for TraceContext
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if let Some(ctx) = parts.extensions.get::<TraceContext>() {
            Ok(ctx.clone())
        } else {
            Ok(TraceContext::generate())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observability_trace_context_validates_and_roundtrips_traceparent_and_tracestate() {
        assert!(!TraceContext::is_valid_traceparent(
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01"
        ));
        assert!(!TraceContext::is_valid_traceparent(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01"
        ));
        assert!(!TraceContext::is_valid_traceparent("invalid"));

        let parent = TraceContext {
            traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
            tracestate: Some("rojo=1,congo=2".to_string()),
        };

        let serialized = parent.to_serialized();
        assert_eq!(
            serialized,
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01\nrojo=1,congo=2"
        );

        let parsed = TraceContext::parse_serialized(&serialized).expect("must parse successfully");
        assert_eq!(parsed.traceparent, parent.traceparent);
        assert_eq!(parsed.tracestate.as_deref(), Some("rojo=1,congo=2"));

        let otel_cx = parsed.to_otel_context();
        let roundtripped =
            TraceContext::from_otel_context(&otel_cx).expect("valid otel span context");
        assert_eq!(roundtripped.trace_id(), parent.trace_id());
        assert_eq!(roundtripped.span_id(), parent.span_id());
        assert_eq!(roundtripped.tracestate.as_deref(), Some("rojo=1,congo=2"));

        let child = parsed.new_child();
        assert_eq!(child.trace_id(), parent.trace_id());
        assert_ne!(child.span_id(), parent.span_id());
        assert_eq!(child.tracestate.as_deref(), Some("rojo=1,congo=2"));

        let single_line = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let parsed_single =
            TraceContext::parse_serialized(single_line).expect("must parse single line");
        assert_eq!(parsed_single.traceparent, single_line);
        assert_eq!(parsed_single.tracestate, None);
    }
}
