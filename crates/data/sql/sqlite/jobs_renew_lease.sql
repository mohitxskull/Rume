UPDATE rume_jobs
SET lease_expires_at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) + ?1
WHERE id = ?2
  AND lease_token = ?3
  AND status = 'in_flight'
  AND lease_expires_at > CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER);
