-- Schema DDL for rume_feature_flags in MySQL.
CREATE TABLE IF NOT EXISTS rume_feature_flags (
    id          VARCHAR(255) NOT NULL PRIMARY KEY,
    tenant_id   VARCHAR(128),
    flag_key    VARCHAR(255) NOT NULL,
    enabled     TINYINT(1)   NOT NULL DEFAULT 0,
    rollout_pct INT          NOT NULL DEFAULT 100,
    rules       JSON,
    created_at  DATETIME(6)  NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
    updated_at  DATETIME(6)  NOT NULL DEFAULT CURRENT_TIMESTAMP(6) ON UPDATE CURRENT_TIMESTAMP(6),
    UNIQUE KEY rume_feature_flags_tenant_key (tenant_id, flag_key)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
