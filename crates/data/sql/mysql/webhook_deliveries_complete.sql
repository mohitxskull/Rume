UPDATE rume_webhook_deliveries
SET status = ?,
    attempt = ?,
    next_attempt_at = CAST(? AS DATETIME(6)),
    last_response = ?,
    updated_at = CAST(? AS DATETIME(6))
WHERE id = ?
  AND status = 'in_flight'
  AND ((? IS NULL AND next_attempt_at IS NULL) OR next_attempt_at = CAST(? AS DATETIME(6)));
