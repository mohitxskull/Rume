UPDATE rume_jobs
SET status = 'dead_letter',
    assigned_worker = NULL,
    lease_expires_at = NULL,
    lease_token = NULL,
    dead_letter_reason = ?1,
    dead_lettered_at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
WHERE id = ?2
  AND lease_token = ?3
  AND status = 'in_flight'
  AND lease_expires_at > CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER);
