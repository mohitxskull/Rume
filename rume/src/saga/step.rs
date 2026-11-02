//! Saga step contracts and functional implementations.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::saga::error::SagaError;

/// Pinned boxed future for saga actions and compensations.
pub type SagaFuture<'a> = Pin<Box<dyn Future<Output = Result<(), SagaError>> + Send + 'a>>;

/// A reversible transaction step within a saga workflow.
pub trait Step<Ctx: Send + Sync + 'static>: Send + Sync {
    /// Name of this step (used for logging and telemetry).
    fn name(&self) -> &str;

    /// Executes the forward action of this step, mutating the shared context.
    fn execute<'a>(&'a self, ctx: &'a mut Ctx) -> SagaFuture<'a>;

    /// Executes the compensating action to undo changes if a subsequent step fails.
    fn compensate<'a>(&'a self, ctx: &'a mut Ctx) -> SagaFuture<'a>;
}

type ActionFn<Ctx> = Arc<dyn for<'a> Fn(&'a mut Ctx) -> SagaFuture<'a> + Send + Sync>;
type CompensateFn<Ctx> = Arc<dyn for<'a> Fn(&'a mut Ctx) -> SagaFuture<'a> + Send + Sync>;

/// A functional step defined by async closures.
pub struct FunctionalStep<Ctx> {
    name: String,
    action: ActionFn<Ctx>,
    compensation: CompensateFn<Ctx>,
}

impl<Ctx> Debug for FunctionalStep<Ctx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FunctionalStep")
            .field("name", &self.name)
            .finish()
    }
}

impl<Ctx: Send + Sync + 'static> FunctionalStep<Ctx> {
    /// Creates a new functional step with name, forward action, and compensation hook.
    pub fn new<A, C, FutA, FutC>(name: impl Into<String>, action: A, compensation: C) -> Self
    where
        A: Fn(&mut Ctx) -> FutA + Send + Sync + 'static,
        C: Fn(&mut Ctx) -> FutC + Send + Sync + 'static,
        FutA: Future<Output = Result<(), SagaError>> + Send + 'static,
        FutC: Future<Output = Result<(), SagaError>> + Send + 'static,
    {
        let name_str = name.into();
        Self {
            name: name_str,
            action: Arc::new(move |ctx| Box::pin(action(ctx))),
            compensation: Arc::new(move |ctx| Box::pin(compensation(ctx))),
        }
    }
}

impl<Ctx: Send + Sync + 'static> Step<Ctx> for FunctionalStep<Ctx> {
    fn name(&self) -> &str {
        &self.name
    }

    fn execute<'a>(&'a self, ctx: &'a mut Ctx) -> SagaFuture<'a> {
        (self.action)(ctx)
    }

    fn compensate<'a>(&'a self, ctx: &'a mut Ctx) -> SagaFuture<'a> {
        (self.compensation)(ctx)
    }
}
