UPDATE rume_jobs
SET status = 'pending',
    assigned_worker = NULL,
    lease_expires_at = NULL,
    lease_token = NULL,
    available_at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) + ?1
WHERE id = ?2
  AND lease_token = ?3
  AND status = 'in_flight'
  AND lease_expires_at > CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER);
