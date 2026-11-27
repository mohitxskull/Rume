UPDATE rume_sagas
SET claim_expires_at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) + ?3
WHERE saga_id = ?1
  AND claim_token = ?2
  AND claim_expires_at > CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER);
