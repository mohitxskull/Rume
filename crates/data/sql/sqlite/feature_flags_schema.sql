-- Schema DDL for rume_feature_flags in SQLite.
CREATE TABLE IF NOT EXISTS rume_feature_flags (
    id          TEXT    NOT NULL PRIMARY KEY,
    tenant_id   TEXT,
    flag_key    TEXT    NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 0,
    rollout_pct INTEGER NOT NULL DEFAULT 100,
    rules       TEXT,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS rume_feature_flags_tenant_key
    ON rume_feature_flags (tenant_id, flag_key);
