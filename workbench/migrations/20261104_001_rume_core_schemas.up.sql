CREATE TABLE IF NOT EXISTS rume_outbox (
    id VARCHAR(64) PRIMARY KEY,
    event_type VARCHAR(255) NOT NULL,
    payload BYTEA NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    retry_count INT NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    last_error TEXT,
    next_retry_at BIGINT,
    lease_token VARCHAR(64),
    lease_expires_at BIGINT
);

CREATE INDEX IF NOT EXISTS idx_rume_outbox_pending
ON rume_outbox (status, created_at)
WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS idx_rume_outbox_lease
ON rume_outbox (status, lease_expires_at)
WHERE status = 'inflight';

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
    tenant_id TEXT,
    assigned_worker TEXT,
    available_at BIGINT NOT NULL,
    lease_expires_at BIGINT,
    status TEXT NOT NULL DEFAULT 'pending',
    dead_letter_reason TEXT,
    dead_lettered_at BIGINT,
    created_at BIGINT NOT NULL,
    completed_at BIGINT,
    lease_token TEXT
);

CREATE INDEX IF NOT EXISTS idx_rume_jobs_pending ON rume_jobs (available_at) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS idx_rume_jobs_dead_letter ON rume_jobs (dead_lettered_at) WHERE status = 'dead_letter';
CREATE UNIQUE INDEX IF NOT EXISTS idx_rume_jobs_idempotency ON rume_jobs (COALESCE(tenant_id, ''), idempotency_key) WHERE idempotency_key IS NOT NULL AND status NOT IN ('dead_letter', 'purged');

CREATE TABLE IF NOT EXISTS rume_locks (
    resource TEXT PRIMARY KEY,
    lease_id TEXT NOT NULL,
    fencing_token BIGINT NOT NULL,
    expires_at BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_rume_locks_expires_at ON rume_locks (expires_at);
CREATE SEQUENCE IF NOT EXISTS rume_lock_fencing_seq START 1;

CREATE TABLE IF NOT EXISTS rume_leaders (
    role TEXT PRIMARY KEY,
    candidate_id TEXT NOT NULL,
    term BIGINT NOT NULL,
    expires_at BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_rume_leaders_expires_at ON rume_leaders (expires_at);
CREATE SEQUENCE IF NOT EXISTS rume_leader_term_seq START 1;
