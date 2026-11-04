use rume::persistence::migration::Migrator;

/// Builds the database migrator for the Workbench reference application.
pub fn workbench_migrator() -> Migrator {
    rume::embed_migrations!("migrations").with_rume_core("postgres")
}
