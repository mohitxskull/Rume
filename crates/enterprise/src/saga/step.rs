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

    /// Executes the forward action with a stable per-saga/step idempotency key.
    ///
    /// Implementations that perform external side effects should use `idempotency_key` to
    /// reconcile or deduplicate retries if a post-step persistence write fails.
    fn execute_with_key<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        _idempotency_key: &'a str,
    ) -> SagaFuture<'a> {
        self.execute(ctx)
    }

    /// Executes the compensating action to undo changes if a subsequent step fails.
    fn compensate<'a>(&'a self, ctx: &'a mut Ctx) -> SagaFuture<'a>;

    /// Executes the compensating action with a stable per-saga/step idempotency key.
    ///
    /// Implementations that undo external side effects should use `idempotency_key` to
    /// reconcile or deduplicate retries if a post-compensation persistence write fails.
    fn compensate_with_key<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        _idempotency_key: &'a str,
    ) -> SagaFuture<'a> {
        self.compensate(ctx)
    }
}

type ActionFn<Ctx> = Arc<dyn for<'a> Fn(&'a mut Ctx, &'a str) -> SagaFuture<'a> + Send + Sync>;
type CompensateFn<Ctx> = Arc<dyn for<'a> Fn(&'a mut Ctx, &'a str) -> SagaFuture<'a> + Send + Sync>;

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
        Self::with_key(
            name,
            move |ctx, _key| action(ctx),
            move |ctx, _key| compensation(ctx),
        )
    }

    /// Creates a new functional step whose action and compensation receive a stable
    /// per-saga/step idempotency key for deduplicating retries.
    pub fn with_key<A, C, FutA, FutC>(name: impl Into<String>, action: A, compensation: C) -> Self
    where
        A: Fn(&mut Ctx, String) -> FutA + Send + Sync + 'static,
        C: Fn(&mut Ctx, String) -> FutC + Send + Sync + 'static,
        FutA: Future<Output = Result<(), SagaError>> + Send + 'static,
        FutC: Future<Output = Result<(), SagaError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            action: Arc::new(move |ctx, key| Box::pin(action(ctx, key.to_string()))),
            compensation: Arc::new(move |ctx, key| Box::pin(compensation(ctx, key.to_string()))),
        }
    }
}

impl<Ctx: Send + Sync + 'static> Step<Ctx> for FunctionalStep<Ctx> {
    fn name(&self) -> &str {
        &self.name
    }

    fn execute<'a>(&'a self, ctx: &'a mut Ctx) -> SagaFuture<'a> {
        (self.action)(ctx, "")
    }

    fn execute_with_key<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        idempotency_key: &'a str,
    ) -> SagaFuture<'a> {
        (self.action)(ctx, idempotency_key)
    }

    fn compensate<'a>(&'a self, ctx: &'a mut Ctx) -> SagaFuture<'a> {
        (self.compensation)(ctx, "")
    }

    fn compensate_with_key<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        idempotency_key: &'a str,
    ) -> SagaFuture<'a> {
        (self.compensation)(ctx, idempotency_key)
    }
}
