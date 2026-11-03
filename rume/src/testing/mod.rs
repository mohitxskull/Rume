//! Testing helpers and declarative test harness for Rume applications.

pub mod app;
pub mod harness;

pub use app::{TestApp, TestAppBuilder, status};

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub use harness::RumeTestHarness;
