CREATE TABLE IF NOT EXISTS rume_sagas (
    saga_id TEXT PRIMARY KEY,
    saga_name TEXT NOT NULL,
    status_json TEXT NOT NULL,
    completed_steps_json TEXT NOT NULL,
    compensated_steps_json TEXT NOT NULL,
    is_terminal INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    claim_token TEXT,
    claim_expires_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_rume_sagas_incomplete ON rume_sagas (is_terminal, updated_at);
