UPDATE rume_jobs
SET status = 'pending',
    assigned_worker = NULL,
    lease_expires_at = NULL,
    lease_token = NULL,
    available_at = FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000) + ?
WHERE id = ?
  AND lease_token = ?
  AND status = 'in_flight'
  AND lease_expires_at > FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000);
