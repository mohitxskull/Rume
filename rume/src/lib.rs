//! Rume: an explicit, typed, production-ready application shell for Axum.
//!
//! This crate is the core of the Rume framework. It is intentionally small:
//! optional features (auth, mail, jobs, storage, database helpers) live in
//! separate `rume-*` crates that depend on this one.

#![forbid(unsafe_code)]

pub mod app;
pub mod config;
pub mod env;
pub mod error;
pub mod validation;

pub use app::Rume;
pub use app::RumeBuilder;
pub use error::AppError;
pub use validation::Validated;

pub use axum;
pub use serde;
pub use tokio;
pub use tracing;
