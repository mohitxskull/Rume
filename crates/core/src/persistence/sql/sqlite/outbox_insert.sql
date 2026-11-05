INSERT INTO rume_outbox (
    id, event_type, payload, status, retry_count, created_at,
    last_error, next_retry_at, lease_token, lease_expires_at,
    trace_context
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
ON CONFLICT(id) DO NOTHING;
