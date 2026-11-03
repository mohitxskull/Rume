CREATE TABLE IF NOT EXISTS rume_locks (
    resource TEXT PRIMARY KEY,
    lease_id TEXT NOT NULL,
    fencing_token BIGINT NOT NULL,
    expires_at BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_rume_locks_expires_at ON rume_locks (expires_at);
CREATE SEQUENCE IF NOT EXISTS rume_lock_fencing_seq START 1;
