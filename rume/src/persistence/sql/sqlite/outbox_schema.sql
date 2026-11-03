CREATE TABLE IF NOT EXISTS rume_outbox (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    payload BLOB NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    retry_count INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    last_error TEXT,
    next_retry_at INTEGER,
    lease_token TEXT,
    lease_expires_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_rume_outbox_pending
ON rume_outbox (status, created_at);

CREATE INDEX IF NOT EXISTS idx_rume_outbox_lease
ON rume_outbox (status, lease_expires_at);

