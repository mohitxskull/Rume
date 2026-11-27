UPDATE rume_webhook_deliveries
SET status = $1,
    attempt = $2,
    next_attempt_at = $3::timestamptz,
    last_response = $4,
    updated_at = $5::timestamptz
WHERE id = $6 AND status = 'in_flight';
