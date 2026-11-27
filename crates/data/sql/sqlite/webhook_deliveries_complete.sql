UPDATE rume_webhook_deliveries
SET status = ?1,
    attempt = ?2,
    next_attempt_at = ?3,
    last_response = ?4,
    updated_at = ?5
WHERE id = ?6
  AND status = 'in_flight'
  AND ((?7 IS NULL AND next_attempt_at IS NULL) OR next_attempt_at = ?7);
