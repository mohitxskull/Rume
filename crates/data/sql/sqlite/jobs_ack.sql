UPDATE rume_jobs
SET status = 'completed',
    completed_at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER),
    lease_expires_at = NULL
WHERE id = ?1
  AND lease_token = ?2
  AND status = 'in_flight'
  AND lease_expires_at > CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER);
