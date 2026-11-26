UPDATE rume_jobs
SET status = 'pending',
    attempt = 0,
    assigned_worker = NULL,
    lease_expires_at = NULL,
    lease_token = NULL,
    dead_letter_reason = NULL,
    dead_lettered_at = NULL,
    available_at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
WHERE id = ?1
  AND status = 'dead_letter';
