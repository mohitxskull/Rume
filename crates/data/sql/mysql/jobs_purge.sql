UPDATE rume_jobs
SET status = 'purged'
WHERE status = 'completed'
  AND completed_at <= FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000) - ?;
