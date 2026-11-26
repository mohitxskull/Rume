UPDATE rume_webhook_deliveries
SET status = 'in_flight',
    next_attempt_at = $2::timestamptz,
    updated_at = $1::timestamptz
WHERE id IN (
    SELECT id FROM rume_webhook_deliveries
    WHERE (status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= $1::timestamptz))
       OR (status = 'in_flight' AND next_attempt_at IS NOT NULL AND next_attempt_at <= $1::timestamptz)
    ORDER BY created_at ASC
    LIMIT $3
    FOR UPDATE SKIP LOCKED
)
RETURNING
    id,
    endpoint_id,
    event_type,
    payload::text AS payload,
    status,
    attempt,
    to_char(next_attempt_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS next_attempt_at,
    last_response,
    to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS created_at,
    to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS updated_at;
