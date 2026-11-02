SELECT id, name, payload, attempt, max_attempts, initial_backoff_ms,
       max_backoff_ms, backoff_factor, timeout_ms, idempotency_key,
       assigned_worker, available_at_ms, created_at_ms
FROM rume_jobs
WHERE available_at_ms <= $1
ORDER BY available_at_ms ASC, created_at_ms ASC
LIMIT 1
FOR UPDATE SKIP LOCKED;
