CREATE TABLE IF NOT EXISTS rume_leaders (
    role TEXT PRIMARY KEY,
    candidate_id TEXT NOT NULL,
    term BIGINT NOT NULL,
    expires_at BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_rume_leaders_expires_at ON rume_leaders (expires_at);
CREATE SEQUENCE IF NOT EXISTS rume_leader_term_seq START 1;
