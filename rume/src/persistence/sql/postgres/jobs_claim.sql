UPDATE rume_jobs
SET status = 'in_flight',
    assigned_worker = $1,
    attempt = attempt + 1,
    lease_expires_at = $2,
    lease_token = $4
WHERE id IN (
    SELECT id FROM rume_jobs
    WHERE (status = 'pending' AND available_at <= $3)
       OR (status = 'in_flight' AND lease_expires_at <= $3)
    ORDER BY available_at ASC, created_at ASC
    LIMIT 1
    FOR UPDATE SKIP LOCKED
)
RETURNING id, name, payload, attempt, max_attempts, initial_backoff_ms, max_backoff_ms, backoff_factor, timeout_ms, idempotency_key, tenant_id, assigned_worker, available_at, lease_token;
