SELECT
    id,
    endpoint_id,
    event_type,
    payload::text AS payload,
    status,
    attempt,
    to_char(next_attempt_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS next_attempt_at,
    last_response,
    to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS created_at,
    to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS updated_at
FROM rume_webhook_deliveries
WHERE endpoint_id = $1
ORDER BY created_at ASC;
