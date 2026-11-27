UPDATE rume_job_batches
SET then_dispatched = TRUE
WHERE id = $1
  AND then_dispatched = FALSE;
