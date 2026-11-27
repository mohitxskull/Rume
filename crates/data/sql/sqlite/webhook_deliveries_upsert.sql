INSERT INTO rume_webhook_deliveries (
    id, endpoint_id, event_type, payload, status, attempt, next_attempt_at, last_response, created_at, updated_at
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
ON CONFLICT(id) DO UPDATE SET
    status = excluded.status,
    attempt = excluded.attempt,
    next_attempt_at = excluded.next_attempt_at,
    last_response = excluded.last_response,
    updated_at = excluded.updated_at;
