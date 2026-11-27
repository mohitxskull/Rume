UPDATE rume_sagas
SET claim_token = ?,
    claim_expires_at = FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000) + ?
WHERE saga_id = ?
  AND (claim_token IS NULL OR claim_expires_at <= FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000));
