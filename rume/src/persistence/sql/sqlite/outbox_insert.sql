INSERT INTO rume_outbox (
    id, event_type, payload, status, retry_count, created_at,
    last_error, next_retry_at, lease_token, lease_expires_at
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
ON CONFLICT(id) DO UPDATE SET
    status = excluded.status,
    retry_count = excluded.retry_count,
    last_error = excluded.last_error,
    next_retry_at = excluded.next_retry_at,
    lease_token = excluded.lease_token,
    lease_expires_at = excluded.lease_expires_at;
