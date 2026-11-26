SELECT id, endpoint_id, event_type, payload, status, attempt, next_attempt_at, last_response, created_at, updated_at
FROM rume_webhook_deliveries
WHERE status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)
ORDER BY created_at ASC
LIMIT ?2;
