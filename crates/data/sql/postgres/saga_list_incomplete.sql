SELECT saga_id, saga_name, status_json, completed_steps_json, compensated_steps_json, updated_at, version
FROM rume_sagas
WHERE is_terminal = FALSE
ORDER BY updated_at ASC;
