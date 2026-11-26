//! Enterprise SAML/SCIM integrations and Saga workflow orchestration for Rume.

#![forbid(unsafe_code)]

pub mod enterprise;
pub mod saga;
#[cfg(feature = "saml")]
pub mod saml;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use rume_observability::TraceContext;
    use tracing_subscriber::layer::SubscriberExt as _;

    use crate::saga::{Saga, SagaError};

    #[tokio::test]
    async fn enterprise_saga_coordinator_links_step_and_compensation_spans_to_parent_trace() {
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("rume-enterprise-test");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        let subscriber = tracing_subscriber::registry().with(otel_layer);
        let _guard = tracing::subscriber::set_default(subscriber);

        let parent_tc = TraceContext::generate();
        let expected_trace_id = parent_tc.trace_id().to_string();

        #[derive(Clone, Default)]
        struct Ctx {
            observed_traces: Arc<Mutex<Vec<(&'static str, String)>>>,
        }

        let saga = Saga::<Ctx>::new("order-checkout")
            .step(
                "reserve-inventory",
                |ctx| {
                    let traces = Arc::clone(&ctx.observed_traces);
                    async move {
                        let tc = TraceContext::current().expect("step 1 must have TraceContext");
                        traces
                            .lock()
                            .unwrap()
                            .push(("step1", tc.trace_id().to_string()));
                        Ok(())
                    }
                },
                |ctx| {
                    let traces = Arc::clone(&ctx.observed_traces);
                    async move {
                        let tc = TraceContext::current().expect("comp 1 must have TraceContext");
                        traces
                            .lock()
                            .unwrap()
                            .push(("comp1", tc.trace_id().to_string()));
                        Ok(())
                    }
                },
            )
            .step(
                "charge-payment",
                |ctx| {
                    let traces = Arc::clone(&ctx.observed_traces);
                    async move {
                        let tc = TraceContext::current().expect("step 2 must have TraceContext");
                        traces
                            .lock()
                            .unwrap()
                            .push(("step2", tc.trace_id().to_string()));
                        Err(SagaError::ActionFailed {
                            step: "charge-payment".into(),
                            message: "card declined".into(),
                        })
                    }
                },
                |_ctx| async { Ok(()) },
            );

        let root_span = tracing::info_span!("caller.root");
        let report = parent_tc
            .scope_future(root_span, saga.run(Ctx::default()))
            .await
            .expect("saga run should return compensated report");
        assert!(!report.status.is_success());

        let recorded = report.context.observed_traces.lock().unwrap().clone();
        assert_eq!(recorded.len(), 3);
        for (phase, trace_id) in recorded {
            assert_eq!(
                trace_id, expected_trace_id,
                "{phase} must share caller trace_id"
            );
        }
    }
}
