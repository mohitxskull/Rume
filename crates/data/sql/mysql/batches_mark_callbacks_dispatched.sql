UPDATE rume_job_batches
SET callbacks_dispatched = 1
WHERE id = ?
  AND callbacks_dispatched = 0;
