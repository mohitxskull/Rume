-- Schema DDL for rume_feature_flags in PostgreSQL.
CREATE TABLE IF NOT EXISTS rume_feature_flags (
    id          TEXT        NOT NULL PRIMARY KEY,
    tenant_id   TEXT,
    flag_key    TEXT        NOT NULL,
    enabled     BOOLEAN     NOT NULL DEFAULT FALSE,
    rollout_pct INTEGER     NOT NULL DEFAULT 100,
    rules       JSONB,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS rume_feature_flags_tenant_key
    ON rume_feature_flags (COALESCE(tenant_id, ''), flag_key);
