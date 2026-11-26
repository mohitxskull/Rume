CREATE TABLE IF NOT EXISTS rume_jobs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    queue TEXT NOT NULL DEFAULT 'default',
    priority INTEGER NOT NULL DEFAULT 0,
    payload BLOB NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    initial_backoff_ms INTEGER NOT NULL DEFAULT 1000,
    max_backoff_ms INTEGER NOT NULL DEFAULT 60000,
    backoff_factor REAL NOT NULL DEFAULT 2.0,
    timeout_ms INTEGER,
    idempotency_key TEXT,
    tenant_id TEXT,
    assigned_worker TEXT,
    available_at INTEGER NOT NULL,
    lease_expires_at INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',
    dead_letter_reason TEXT,
    dead_lettered_at INTEGER,
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    lease_token TEXT,
    trace_context TEXT
);

CREATE INDEX IF NOT EXISTS idx_rume_jobs_pending ON rume_jobs (queue, priority DESC, available_at ASC, created_at ASC) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS idx_rume_jobs_dead_letter ON rume_jobs (dead_lettered_at) WHERE status = 'dead_letter';
CREATE UNIQUE INDEX IF NOT EXISTS idx_rume_jobs_idempotency ON rume_jobs (COALESCE(tenant_id, ''), idempotency_key) WHERE idempotency_key IS NOT NULL AND status NOT IN ('dead_letter', 'purged');
