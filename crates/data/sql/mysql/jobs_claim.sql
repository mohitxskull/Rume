SELECT id, name, payload, attempt, max_attempts, initial_backoff_ms, max_backoff_ms, backoff_factor, timeout_ms, idempotency_key, tenant_id, assigned_worker, available_at, lease_token, trace_context, queue, priority
FROM rume_jobs
WHERE (status = 'pending' AND available_at <= FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000))
   OR (status = 'in_flight' AND lease_expires_at <= FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000))
ORDER BY priority DESC, available_at ASC, created_at ASC
LIMIT 1
FOR UPDATE SKIP LOCKED;
