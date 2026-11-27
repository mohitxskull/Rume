INSERT INTO rume_feature_flags (id, tenant_id, flag_key, enabled, rollout_pct, rules)
VALUES (?, ?, ?, ?, ?, ?) AS new
ON DUPLICATE KEY UPDATE
    enabled = new.enabled,
    rollout_pct = new.rollout_pct,
    rules = new.rules,
    updated_at = CURRENT_TIMESTAMP(6);
