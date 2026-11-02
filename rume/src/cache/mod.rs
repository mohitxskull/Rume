//! Typed caching interface, cache-aside pattern, tag invalidation, and health diagnostics.

pub mod backend;
pub mod config;
pub mod error;
pub mod typed;

pub use backend::{BoxFuture, CacheBackend, MemoryCacheBackend};
pub use config::CacheConfig;
pub use error::CacheError;
pub use typed::{Cache, CacheHealthCheck};
