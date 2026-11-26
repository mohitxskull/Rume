UPDATE rume_jobs
SET status = 'dead_letter',
    assigned_worker = NULL,
    lease_expires_at = NULL,
    lease_token = NULL,
    dead_letter_reason = ?,
    dead_lettered_at = FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000)
WHERE id = ?
  AND lease_token = ?
  AND status = 'in_flight'
  AND lease_expires_at > FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000);
