INSERT INTO rume_outbox (
    id, event_type, payload, status, retry_count, created_at,
    last_error, next_retry_at, lease_token, lease_expires_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON DUPLICATE KEY UPDATE
    status = VALUES(status),
    retry_count = VALUES(retry_count),
    last_error = VALUES(last_error),
    next_retry_at = VALUES(next_retry_at),
    lease_token = VALUES(lease_token),
    lease_expires_at = VALUES(lease_expires_at);
