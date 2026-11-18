CREATE TABLE IF NOT EXISTS rume_audit_log (
    id TEXT PRIMARY KEY,
    action TEXT NOT NULL,
    resource TEXT NOT NULL,
    status TEXT NOT NULL,
    actor_id TEXT,
    target_id TEXT,
    ip_address TEXT,
    metadata TEXT,
    created_at INTEGER NOT NULL
);
