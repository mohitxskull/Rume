//! First-party persistence integration.
//!
//! Provides connection pool lifecycle management, transactional boundaries,
//! health checks, and database migrations.

pub mod backend;
pub mod config;
pub mod error;
pub mod migration;
pub mod pool;
#[cfg(feature = "postgres")]
pub mod postgres;

pub use backend::{BoxFuture, DatabaseBackend, DatabaseTransaction, MemoryDatabase};
pub use config::DatabaseConfig;
pub use error::PersistenceError;
pub use migration::{Migration, MigrationSummary, Migrator};
pub use pool::{ConnectionLease, DatabaseHealthCheck, DatabasePool, PoolStatus};
#[cfg(feature = "postgres")]
pub use postgres::{PostgresBackend, PostgresTransaction};
