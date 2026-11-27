UPDATE rume_jobs
SET status = 'pending',
    attempt = 0,
    assigned_worker = NULL,
    lease_expires_at = NULL,
    lease_token = NULL,
    dead_letter_reason = NULL,
    dead_lettered_at = NULL,
    available_at = FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000)
WHERE id = ?
  AND status = 'dead_letter';
