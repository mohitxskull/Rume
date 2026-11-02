CREATE TABLE IF NOT EXISTS rume_jobs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    payload BYTEA NOT NULL,
    attempt INT NOT NULL DEFAULT 0,
    max_attempts INT NOT NULL DEFAULT 3,
    initial_backoff_ms BIGINT NOT NULL DEFAULT 1000,
    max_backoff_ms BIGINT NOT NULL DEFAULT 60000,
    backoff_factor DOUBLE PRECISION NOT NULL DEFAULT 2.0,
    timeout_ms BIGINT,
    idempotency_key TEXT,
    assigned_worker TEXT,
    available_at BIGINT NOT NULL,
    lease_expires_at BIGINT,
    status TEXT NOT NULL DEFAULT 'pending',
    dead_letter_reason TEXT,
    created_at BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_rume_jobs_pending ON rume_jobs (available_at) WHERE status = 'pending';
CREATE UNIQUE INDEX IF NOT EXISTS idx_rume_jobs_idempotency ON rume_jobs (idempotency_key) WHERE idempotency_key IS NOT NULL AND status != 'dead_letter';
