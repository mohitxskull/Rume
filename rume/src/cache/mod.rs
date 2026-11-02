//! Typed caching interface, cache-aside pattern, tag invalidation, and health diagnostics.

pub mod backend;
pub mod config;
pub mod error;
#[cfg(feature = "redis")]
pub mod redis;
pub mod typed;

#[cfg(feature = "redis")]
pub use self::redis::RedisCacheBackend;
pub use backend::{BoxFuture, CacheBackend, MemoryCacheBackend};
pub use config::CacheConfig;
pub use error::CacheError;
pub use typed::{Cache, CacheHealthCheck};
