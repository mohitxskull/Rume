UPDATE rume_jobs
SET lease_expires_at = FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000) + ?
WHERE id = ?
  AND lease_token = ?
  AND status = 'in_flight'
  AND lease_expires_at > FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000);
