INSERT INTO rume_jobs (
    id, name, payload, attempt, max_attempts, initial_backoff_ms,
    max_backoff_ms, backoff_factor, timeout_ms, idempotency_key,
    available_at, status, created_at
) VALUES ($1, $2, $3, 0, $4, $5, $6, $7, $8, $9, $10, 'pending', $11)
ON CONFLICT (idempotency_key) WHERE idempotency_key IS NOT NULL AND status != 'dead_letter'
DO NOTHING
RETURNING id;
