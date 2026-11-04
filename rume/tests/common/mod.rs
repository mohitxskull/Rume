//! Shared test fixtures, container lifecycles, and test assertions.

use std::sync::Mutex;

pub mod containers;

/// Shared environment lock for tests that mutate process environment variables.
pub static ENV_LOCK: Mutex<()> = Mutex::new(());
