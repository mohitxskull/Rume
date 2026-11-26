INSERT IGNORE INTO rume_jobs (
    id, name, queue, priority, payload, attempt, max_attempts, initial_backoff_ms,
    max_backoff_ms, backoff_factor, timeout_ms, idempotency_key,
    tenant_id, available_at, status, created_at, trace_context
) VALUES (
    ?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?, ?,
    FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000) + ?,
    'pending',
    FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000),
    ?
);
