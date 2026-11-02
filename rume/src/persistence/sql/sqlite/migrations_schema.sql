CREATE TABLE IF NOT EXISTS rume_migrations (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at INTEGER NOT NULL,
    checksum TEXT NOT NULL
);
