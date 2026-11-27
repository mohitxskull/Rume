UPDATE rume_job_batches
SET then_dispatched = 1,
    finally_dispatched = 1
WHERE id = ?1
  AND then_dispatched = 0
  AND finally_dispatched = 0;
