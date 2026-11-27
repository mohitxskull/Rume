SELECT id, tenant_id, metric, count
FROM rume_usage_counters
WHERE count > 0
ORDER BY tenant_id ASC, metric ASC;
