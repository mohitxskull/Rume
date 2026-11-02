CREATE TABLE IF NOT EXISTS rume_locks (
    resource TEXT PRIMARY KEY,
    lease_id TEXT NOT NULL,
    fencing_token BIGINT NOT NULL,
    expires_at BIGINT NOT NULL
);
CREATE SEQUENCE IF NOT EXISTS rume_lock_fencing_seq START 1;
