//! Testing helpers and declarative test harness for Rume applications.

pub mod app;
pub mod events;
pub mod fixtures;
pub mod harness;
pub mod jobs;
#[cfg(feature = "testing")]
pub mod server;

pub use app::{TestApp, TestAppBuilder, TestResponse, status};
pub use events::TestEventBus;
#[cfg(feature = "postgres")]
pub use fixtures::PostgresSchemaFixture;
#[cfg(feature = "sqlite")]
pub use fixtures::SqliteTestFixture;
pub use fixtures::TestTransactionFixture;
pub use jobs::TestJobQueue;
#[cfg(feature = "testing")]
pub use server::TestServer;

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub use harness::RumeTestHarness;

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub mod conformance;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub use conformance::{ConformanceRecord, verify_database_conformance};
