INSERT IGNORE INTO rume_sagas (
    saga_id, saga_name, status_json, completed_steps_json, compensated_steps_json,
    is_terminal, updated_at, version, claim_token, claim_expires_at
) VALUES (
    ?, ?, ?, ?, ?, ?, ?, 1, ?,
    CASE WHEN ? IS NOT NULL
        THEN FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000) + ?
        ELSE NULL
    END
);
