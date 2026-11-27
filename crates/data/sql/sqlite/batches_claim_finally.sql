UPDATE rume_job_batches
SET finally_dispatched = 1
WHERE id = ?1
  AND finally_dispatched = 0;
