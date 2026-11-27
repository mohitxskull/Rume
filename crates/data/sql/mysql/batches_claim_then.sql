UPDATE rume_job_batches
SET then_dispatched = 1
WHERE id = ?
  AND then_dispatched = 0;
