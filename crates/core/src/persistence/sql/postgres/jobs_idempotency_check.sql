SELECT id FROM rume_jobs WHERE COALESCE(tenant_id, '') = COALESCE($1, '') AND idempotency_key = $2 AND status NOT IN ('dead_letter', 'purged')
