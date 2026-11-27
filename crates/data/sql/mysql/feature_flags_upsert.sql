INSERT INTO rume_feature_flags (id, tenant_id, flag_key, enabled, rollout_pct, rules)
VALUES (?, ?, ?, ?, ?, ?)
ON DUPLICATE KEY UPDATE
    enabled = VALUES(enabled),
    rollout_pct = VALUES(rollout_pct),
    rules = VALUES(rules),
    updated_at = CURRENT_TIMESTAMP(6);
