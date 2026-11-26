//! First-party persistence integration.
//!
//! Provides connection pool lifecycle management, transactional boundaries,
//! health checks, and database migrations.

pub mod backend;
pub mod builder;
pub mod config;
pub mod conventions;
pub mod error;
pub mod migration;
#[cfg(feature = "mysql")]
pub mod mysql;
pub mod pagination;
pub mod pool;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod query;
pub mod read_write;
pub mod row;
pub mod seed;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod tenancy;

pub use backend::{
    BoxFuture, DatabaseBackend, DatabaseTransaction, DbValue, validate_sql_identifier,
    validate_tenant_id,
};
pub use builder::{
    BuiltQuery, DynamicDelete, DynamicInsert, DynamicSelect, DynamicUpdate, SqlDialect,
};
pub use config::DatabaseConfig;
pub use conventions::{
    OptimisticLock, SoftDeletable, execute_cas, execute_cas_tx, soft_delete_update,
    where_not_deleted, where_only_deleted, with_version_check,
};
pub use error::PersistenceError;
pub use migration::{Migration, MigrationSummary, Migrator, RUME_AUTH_MIGRATION_ID};
#[cfg(feature = "mysql")]
pub use mysql::{MariadbBackend, MariadbTransaction, MysqlBackend, MysqlTransaction};
pub use pagination::{CursorCodec, CursorPage, CursorPageRequest, OffsetPage, OffsetPageRequest};
pub use pool::{
    ConnectionLease, DatabaseHealthCheck, DatabasePool, MetricsRegistryPoolExt, PoolStatus,
};
#[cfg(feature = "postgres")]
pub use postgres::{PostgresBackend, PostgresTransaction};
pub use query::SqlQuery;
pub use read_write::{ReadWriteConfig, ReadWriteHealthCheck, ReadWritePool, ReadWritePoolStatus};
pub use row::{DbRow, FromDbValue, FromRow};
#[cfg(feature = "sqlite")]
pub use sqlite::{SqliteBackend, SqliteTransaction};
pub use tenancy::{TenantIsolationStrategy, TenantPoolManager};
