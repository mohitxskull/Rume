SELECT id, tenant_id, flag_key, enabled, rollout_pct, rules
FROM rume_feature_flags
WHERE id = ?;
