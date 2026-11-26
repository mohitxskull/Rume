UPDATE rume_sagas
SET saga_name = ?2,
    status_json = ?3,
    completed_steps_json = ?4,
    compensated_steps_json = ?5,
    is_terminal = ?6,
    updated_at = ?7,
    version = version + 1
WHERE saga_id = ?1
  AND version = ?8;
