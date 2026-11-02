//! Distributed coordination primitives: leases, fencing locks, and leader election.

pub mod error;
pub mod leader;
pub mod lock;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "redis")]
pub mod redis;

#[cfg(feature = "redis")]
pub use self::redis::{RedisDistributedLock, RedisLeaderElector};
pub use error::CoordinationError;
pub use leader::{LeaderElector, LeaderLease, MemoryLeaderElector};
pub use lock::{CoordinationFuture, DistributedLock, LockGuard, MemoryDistributedLock};
#[cfg(feature = "postgres")]
pub use postgres::{PostgresDistributedLock, PostgresLeaderElector};
