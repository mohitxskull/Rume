INSERT INTO rume_outbox (
    id, event_type, payload, status, retry_count, created_at,
    last_error, next_retry_at, lease_token, lease_expires_at
) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
ON CONFLICT (id) DO UPDATE SET
    status = EXCLUDED.status,
    retry_count = EXCLUDED.retry_count,
    last_error = EXCLUDED.last_error,
    next_retry_at = EXCLUDED.next_retry_at,
    lease_token = EXCLUDED.lease_token,
    lease_expires_at = EXCLUDED.lease_expires_at;
