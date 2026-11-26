UPDATE rume_webhook_deliveries
SET status = 'in_flight',
    next_attempt_at = ?2,
    updated_at = ?1
WHERE id IN (
    SELECT id FROM rume_webhook_deliveries
    WHERE (status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= ?1))
       OR (status = 'in_flight' AND next_attempt_at IS NOT NULL AND next_attempt_at <= ?1)
    ORDER BY created_at ASC
    LIMIT ?3
)
RETURNING id, endpoint_id, event_type, payload, status, attempt, next_attempt_at, last_response, created_at, updated_at;
