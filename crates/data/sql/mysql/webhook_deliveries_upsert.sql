INSERT INTO rume_webhook_deliveries (
    id, endpoint_id, event_type, payload, status, attempt, next_attempt_at, last_response, created_at, updated_at
) VALUES (?, ?, ?, CAST(? AS JSON), ?, ?, CAST(? AS DATETIME(6)), ?, CAST(? AS DATETIME(6)), CAST(? AS DATETIME(6)))
ON DUPLICATE KEY UPDATE
    status = VALUES(status),
    attempt = VALUES(attempt),
    next_attempt_at = VALUES(next_attempt_at),
    last_response = VALUES(last_response),
    updated_at = VALUES(updated_at);
