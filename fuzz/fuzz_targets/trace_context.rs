#![no_main]

use libfuzzer_sys::fuzz_target;
use rume_observability::trace_context::TraceContext;

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = std::str::from_utf8(data) {
        let tc = TraceContext::from_traceparent(input);
        let _ = tc.to_otel_context();
        let _ = TraceContext::parse_serialized(input);
    }
});
