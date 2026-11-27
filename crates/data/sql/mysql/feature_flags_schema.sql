-- Schema DDL for rume_feature_flags in MySQL.
CREATE TABLE IF NOT EXISTS rume_feature_flags (
    id          VARCHAR(255) NOT NULL PRIMARY KEY,
    tenant_id   VARCHAR(128),
    tenant_key  VARCHAR(128) GENERATED ALWAYS AS (COALESCE(tenant_id, '')) STORED,
    flag_key    VARCHAR(255) NOT NULL,
    enabled     TINYINT(1)   NOT NULL DEFAULT 0,
    rollout_pct INT          NOT NULL DEFAULT 100,
    rules       JSON,
    created_at  DATETIME(6)  NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
    updated_at  DATETIME(6)  NOT NULL DEFAULT CURRENT_TIMESTAMP(6) ON UPDATE CURRENT_TIMESTAMP(6),
    UNIQUE KEY rume_feature_flags_tenant_key (tenant_key, flag_key)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
