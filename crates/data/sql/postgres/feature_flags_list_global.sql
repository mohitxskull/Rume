SELECT id, tenant_id, flag_key, enabled, rollout_pct, rules::text AS rules
FROM rume_feature_flags
WHERE tenant_id IS NULL
ORDER BY flag_key ASC;
