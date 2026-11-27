SELECT id, tenant_id, flag_key, enabled, rollout_pct, CAST(rules AS CHAR) AS rules
FROM rume_feature_flags
WHERE id = ?;
