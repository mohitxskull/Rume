UPDATE rume_job_batches
SET finally_dispatched = FALSE
WHERE id = $1;
