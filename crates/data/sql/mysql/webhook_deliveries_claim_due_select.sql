SELECT
    id,
    endpoint_id,
    event_type,
    CAST(payload AS CHAR) AS payload,
    status,
    attempt,
    DATE_FORMAT(next_attempt_at, '%Y-%m-%dT%H:%i:%s.%fZ') AS next_attempt_at,
    last_response,
    DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ') AS created_at,
    DATE_FORMAT(updated_at, '%Y-%m-%dT%H:%i:%s.%fZ') AS updated_at
FROM rume_webhook_deliveries
WHERE (status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= CAST(? AS DATETIME(6))))
   OR (status = 'in_flight' AND next_attempt_at IS NOT NULL AND next_attempt_at <= CAST(? AS DATETIME(6)))
ORDER BY created_at ASC
LIMIT ?
FOR UPDATE SKIP LOCKED;
