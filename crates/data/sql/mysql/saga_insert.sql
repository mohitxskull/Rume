INSERT IGNORE INTO rume_sagas (
    saga_id, saga_name, status_json, completed_steps_json, compensated_steps_json,
    is_terminal, updated_at, version
) VALUES (?, ?, ?, ?, ?, ?, ?, 1);
