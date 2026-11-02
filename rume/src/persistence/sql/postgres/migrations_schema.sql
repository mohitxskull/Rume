CREATE TABLE IF NOT EXISTS rume_migrations (
    id VARCHAR(255) PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    applied_at BIGINT NOT NULL,
    checksum VARCHAR(64) NOT NULL
);
