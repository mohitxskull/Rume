//! `async-graphql` extension wrapping execution in a `tracing` span linked to `TraceContext::current()`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_graphql::Response;
use async_graphql::extensions::{Extension, ExtensionContext, ExtensionFactory, NextExecute};
use rume_observability::TraceContext;
use tracing::Instrument as _;

/// `async-graphql` extension that attaches `rume_observability::TraceContext::current()`
/// to a `tracing` span around GraphQL execution.
#[derive(Debug, Clone, Default)]
pub struct RumeTraceExtension;

impl ExtensionFactory for RumeTraceExtension {
    fn create(&self) -> Arc<dyn Extension> {
        Arc::new(RumeTraceExtensionImpl)
    }
}

struct RumeTraceExtensionImpl;

impl Extension for RumeTraceExtensionImpl {
    fn execute<'life0, 'life1, 'life2, 'life3, 'life4, 'async_trait>(
        &'life0 self,
        ctx: &'life1 ExtensionContext<'life2>,
        operation_name: Option<&'life3 str>,
        next: NextExecute<'life4>,
    ) -> Pin<Box<dyn Future<Output = Response> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        'life3: 'async_trait,
        'life4: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let span = tracing::info_span!(
                "graphql.execute",
                operation = operation_name.unwrap_or("<anonymous>")
            );
            if let Some(tc) = TraceContext::current() {
                tc.scope_future(span, next.run(ctx, operation_name)).await
            } else {
                next.run(ctx, operation_name).instrument(span).await
            }
        })
    }
}
