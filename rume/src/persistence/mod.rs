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
#[cfg(feature = "sqlite")]
pub mod sqlite;
#[cfg(feature = "mysql")]
pub mod mysql;

pub use backend::{BoxFuture, DatabaseBackend, DatabaseTransaction};
pub use config::DatabaseConfig;
pub use error::PersistenceError;
pub use migration::{Migration, MigrationSummary, Migrator};
pub use pool::{ConnectionLease, DatabaseHealthCheck, DatabasePool, PoolStatus};
#[cfg(feature = "postgres")]
pub use postgres::{PostgresBackend, PostgresTransaction};
#[cfg(feature = "sqlite")]
pub use sqlite::{SqliteBackend, SqliteTransaction};
#[cfg(feature = "mysql")]
pub use mysql::{MysqlBackend, MysqlTransaction};
