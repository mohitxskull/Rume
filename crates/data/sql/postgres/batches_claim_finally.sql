UPDATE rume_job_batches
SET finally_dispatched = TRUE
WHERE id = $1
  AND finally_dispatched = FALSE;
