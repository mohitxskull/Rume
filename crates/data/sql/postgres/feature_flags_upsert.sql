INSERT INTO rume_feature_flags (id, tenant_id, flag_key, enabled, rollout_pct, rules)
VALUES ($1, $2, $3, $4, $5, $6::jsonb)
ON CONFLICT (id) DO UPDATE SET
    enabled = EXCLUDED.enabled,
    rollout_pct = EXCLUDED.rollout_pct,
    rules = EXCLUDED.rules,
    updated_at = NOW();
