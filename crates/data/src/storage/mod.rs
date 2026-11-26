//! Object storage abstraction and backends.

pub mod local;
#[cfg(feature = "s3")]
pub mod s3;

/// Placeholder unified object storage trait for Milestone C.
pub trait ObjectStore {}
