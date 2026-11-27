UPDATE rume_job_batches
SET callbacks_dispatched = TRUE
WHERE id = $1
  AND callbacks_dispatched = FALSE;
