//! Database persistence, caching, and distributed coordination primitives for Rume.

#![forbid(unsafe_code)]

pub mod cache;
pub mod coordination;
pub mod persistence;
pub mod sql;

pub use persistence::MetricsRegistryPoolExt;
