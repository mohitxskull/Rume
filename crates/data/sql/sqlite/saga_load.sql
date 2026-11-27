SELECT saga_id, saga_name, status_json, completed_steps_json, compensated_steps_json, updated_at, version
FROM rume_sagas
WHERE saga_id = ?1;
