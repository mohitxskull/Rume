INSERT INTO rume_jobs (
    id, name, payload, attempt, max_attempts, initial_backoff_ms,
    max_backoff_ms, backoff_factor, timeout_ms, idempotency_key,
    tenant_id, available_at, status, created_at
) VALUES ($1, $2, $3, 0, $4, $5, $6, $7, $8, $9, $10, $11, 'pending', $12)
ON CONFLICT (COALESCE(tenant_id, ''), idempotency_key) WHERE idempotency_key IS NOT NULL AND status NOT IN ('dead_letter', 'purged')
DO NOTHING
RETURNING id;
