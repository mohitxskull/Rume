INSERT INTO rume_outbox (
    id, event_type, payload, status, retry_count, created_at,
    last_error, next_retry_at, lease_token, lease_expires_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON DUPLICATE KEY UPDATE id = id;
