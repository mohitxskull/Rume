INSERT INTO rume_webhook_deliveries (
    id, endpoint_id, event_type, payload, status, attempt, next_attempt_at, last_response, created_at, updated_at
) VALUES ($1, $2, $3, $4::jsonb, $5, $6, $7::timestamptz, $8, $9::timestamptz, $10::timestamptz)
ON CONFLICT (id) DO UPDATE SET
    status = EXCLUDED.status,
    attempt = EXCLUDED.attempt,
    next_attempt_at = EXCLUDED.next_attempt_at,
    last_response = EXCLUDED.last_response,
    updated_at = EXCLUDED.updated_at;
