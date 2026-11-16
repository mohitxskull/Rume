UPDATE rume_jobs
SET status = 'purged'
WHERE status = 'completed'
  AND completed_at <= (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT - $1;
