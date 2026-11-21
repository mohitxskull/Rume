//! Isolated database and transaction fixtures for test isolation.

#[cfg(feature = "sqlite")]
use rume_data::persistence::DatabaseConfig;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
use rume_data::persistence::{DatabaseBackend, DatabasePool};
use rume_data::persistence::{DatabaseTransaction, PersistenceError};
#[cfg(feature = "sqlite")]
use std::path::{Path, PathBuf};
#[cfg(any(feature = "postgres", all(test, feature = "sqlite")))]
use std::sync::Arc;

#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "sqlite")]
mod sqlite;
#[cfg(all(test, feature = "sqlite"))]
mod tests;
mod transaction;

#[cfg(feature = "postgres")]
pub use postgres::PostgresSchemaFixture;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteTestFixture;
pub use transaction::TestTransactionFixture;
