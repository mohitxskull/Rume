UPDATE rume_jobs
SET status = 'in_flight',
    assigned_worker = ?1,
    attempt = attempt + 1,
    lease_expires_at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) + ?2,
    lease_token = ?3
WHERE id = (
    SELECT id FROM rume_jobs
    WHERE (status = 'pending' AND available_at <= CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
       OR (status = 'in_flight' AND lease_expires_at <= CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
    ORDER BY priority DESC, available_at ASC, created_at ASC
    LIMIT 1
)
RETURNING id, name, payload, attempt, max_attempts, initial_backoff_ms, max_backoff_ms, backoff_factor, timeout_ms, idempotency_key, tenant_id, assigned_worker, available_at, lease_token, trace_context, queue, priority;
