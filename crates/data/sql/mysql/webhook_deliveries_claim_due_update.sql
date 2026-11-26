UPDATE rume_webhook_deliveries
SET status = 'in_flight',
    next_attempt_at = CAST(? AS DATETIME(6)),
    updated_at = CAST(? AS DATETIME(6))
WHERE id = ?;
