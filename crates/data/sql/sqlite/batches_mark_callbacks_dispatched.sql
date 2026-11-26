UPDATE rume_job_batches
SET callbacks_dispatched = 1
WHERE id = ?1
  AND callbacks_dispatched = 0;
