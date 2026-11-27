SELECT id, tenant_id, flag_key, enabled, rollout_pct, rules::text AS rules
FROM rume_feature_flags
WHERE id = $1;
