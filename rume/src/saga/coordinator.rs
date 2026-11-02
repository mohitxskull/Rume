//! Saga workflow coordinator and runner.

use std::fmt::Debug;
use std::sync::Arc;

use crate::saga::error::SagaError;
use crate::saga::step::{FunctionalStep, Step};

/// Final status outcome of a saga workflow execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SagaStatus {
    /// All steps succeeded without errors.
    Success,
    /// A step failed and all previously executed steps were successfully compensated.
    Compensated { error: SagaError },
    /// A step failed and at least one compensation action also failed.
    CompensationFailed {
        error: SagaError,
        compensation_error: SagaError,
    },
}

impl SagaStatus {
    /// Returns true if the saga completed all steps successfully.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }
}

/// Comprehensive report containing the updated context and execution trace.
pub struct SagaReport<Ctx> {
    /// The final mutated context.
    pub context: Ctx,
    /// Final status outcome.
    pub status: SagaStatus,
    /// List of step names that succeeded during forward execution.
    pub completed_steps: Vec<String>,
    /// List of step names that were compensated during rollback.
    pub compensated_steps: Vec<String>,
}

/// A composable multi-step saga workflow with automatic reverse compensation.
pub struct Saga<Ctx: Send + Sync + 'static> {
    name: String,
    steps: Vec<Arc<dyn Step<Ctx>>>,
}

impl<Ctx: Send + Sync + 'static> Saga<Ctx> {
    /// Creates a new saga workflow with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            steps: Vec::new(),
        }
    }

    /// Adds a step implementing [`Step`].
    pub fn add_step<S: Step<Ctx> + 'static>(mut self, step: S) -> Self {
        self.steps.push(Arc::new(step));
        self
    }

    /// Adds a step from async closures.
    pub fn step<A, C, FutA, FutC>(
        mut self,
        name: impl Into<String>,
        action: A,
        compensation: C,
    ) -> Self
    where
        A: Fn(&mut Ctx) -> FutA + Send + Sync + 'static,
        C: Fn(&mut Ctx) -> FutC + Send + Sync + 'static,
        FutA: std::future::Future<Output = Result<(), SagaError>> + Send + 'static,
        FutC: std::future::Future<Output = Result<(), SagaError>> + Send + 'static,
    {
        let s = FunctionalStep::new(name, action, compensation);
        self.steps.push(Arc::new(s));
        self
    }

    /// Returns the name of this saga.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Executes the saga on the provided context, rolling back in reverse order if any step fails.
    pub async fn run(&self, mut ctx: Ctx) -> SagaReport<Ctx> {
        let mut completed_indices = Vec::new();
        let mut completed_names = Vec::new();
        let mut compensated_names = Vec::new();

        for (idx, step) in self.steps.iter().enumerate() {
            match step.execute(&mut ctx).await {
                Ok(()) => {
                    completed_indices.push(idx);
                    completed_names.push(step.name().to_string());
                }
                Err(err) => {
                    // Forward execution failed. Run compensation in reverse order.
                    for &prev_idx in completed_indices.iter().rev() {
                        let prev_step = &self.steps[prev_idx];
                        if let Err(comp_err) = prev_step.compensate(&mut ctx).await {
                            return SagaReport {
                                context: ctx,
                                status: SagaStatus::CompensationFailed {
                                    error: err,
                                    compensation_error: comp_err,
                                },
                                completed_steps: completed_names,
                                compensated_steps: compensated_names,
                            };
                        }
                        compensated_names.push(prev_step.name().to_string());
                    }

                    return SagaReport {
                        context: ctx,
                        status: SagaStatus::Compensated { error: err },
                        completed_steps: completed_names,
                        compensated_steps: compensated_names,
                    };
                }
            }
        }

        SagaReport {
            context: ctx,
            status: SagaStatus::Success,
            completed_steps: completed_names,
            compensated_steps: compensated_names,
        }
    }
}
