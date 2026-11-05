use rume::persistence::migration::Migrator;

/// Builds the database migrator with compile-time embedded migrations and Rume core schema.
pub fn migrator(dialect: &str) -> Migrator {
    rume::embed_migrations!("migrations").with_rume_core(dialect)
}
