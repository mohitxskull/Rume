//! Distributed coordination primitives: leases, fencing locks, and leader election.

pub mod error;
pub mod leader;
pub mod lock;

pub use error::CoordinationError;
pub use leader::{LeaderElector, MemoryLeaderElector};
pub use lock::{CoordinationFuture, DistributedLock, LockGuard, MemoryDistributedLock};
