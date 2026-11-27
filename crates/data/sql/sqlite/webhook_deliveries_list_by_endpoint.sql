SELECT id, endpoint_id, event_type, payload, status, attempt, next_attempt_at, last_response, created_at, updated_at
FROM rume_webhook_deliveries
WHERE endpoint_id = ?1
ORDER BY created_at ASC;
