UPDATE rume_job_batches
SET status = 'cancelled',
    finished_at = COALESCE(finished_at, FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000))
WHERE id = ?
  AND status = 'running';
