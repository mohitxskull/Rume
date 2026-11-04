//! Testing helpers and declarative test harness for Rume applications.

pub mod app;
pub mod events;
pub mod fixtures;
pub mod harness;
pub mod jobs;

pub use app::{TestApp, TestAppBuilder, TestResponse, status};
pub use events::TestEventBus;
pub use fixtures::TestTransactionFixture;
#[cfg(feature = "postgres")]
pub use fixtures::PostgresSchemaFixture;
#[cfg(feature = "sqlite")]
pub use fixtures::SqliteTestFixture;
pub use jobs::TestJobQueue;

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub use harness::RumeTestHarness;
