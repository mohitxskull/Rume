UPDATE rume_sagas
SET saga_name = ?,
    status_json = ?,
    completed_steps_json = ?,
    compensated_steps_json = ?,
    is_terminal = ?,
    updated_at = ?,
    version = version + 1
WHERE saga_id = ?
  AND version = ?;
