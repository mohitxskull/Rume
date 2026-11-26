SELECT id FROM rume_jobs
WHERE COALESCE(tenant_id, '') = COALESCE(?, '')
  AND idempotency_key = ?
  AND status NOT IN ('dead_letter', 'purged');
