use rume::persistence::migration::{Migration, Migrator};

const CORE_UP: &str = include_str!("../migrations/20261104_001_rume_core_schemas.up.sql");
const CORE_DOWN: &str = include_str!("../migrations/20261104_001_rume_core_schemas.down.sql");

const DOMAIN_UP: &str = include_str!("../migrations/20261104_002_workbench_domain_schemas.up.sql");
const DOMAIN_DOWN: &str = include_str!("../migrations/20261104_002_workbench_domain_schemas.down.sql");

const AUTH_UP: &str = include_str!("../migrations/20261104_003_workbench_organizations_and_users.up.sql");
const AUTH_DOWN: &str = include_str!("../migrations/20261104_003_workbench_organizations_and_users.down.sql");

/// Builds the database migrator for the Workbench reference application.
pub fn workbench_migrator() -> Migrator {
    let mut migrator = Migrator::new();

    // Migration 1: Rume Core Schemas (Outbox, Jobs, Locks, Leader)
    migrator = migrator.add_migration(
        Migration::from_sql(
            "20261104_001_rume_core_schemas",
            "Initialize Rume core persistence tables",
            CORE_UP,
        )
        .with_down(CORE_DOWN),
    );

    // Migration 2: Workbench Domain Tables (Projects & Issues)
    migrator = migrator.add_migration(
        Migration::from_sql(
            "20261104_002_workbench_domain_schemas",
            "Create Workbench project and issue tables",
            DOMAIN_UP,
        )
        .with_down(DOMAIN_DOWN),
    );

    // Migration 3: Workbench Organizations & Persisted Users
    migrator = migrator.add_migration(
        Migration::from_sql(
            "20261104_003_workbench_organizations_and_users",
            "Create Workbench organizations and users tables with default seeds",
            AUTH_UP,
        )
        .with_down(AUTH_DOWN),
    );

    migrator
}
