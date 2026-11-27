INSERT INTO rume_sagas (
    saga_id, saga_name, status_json, completed_steps_json, compensated_steps_json,
    is_terminal, updated_at, version, claim_token, claim_expires_at
) VALUES (
    $1, $2, $3, $4, $5, $6, $7, 1, $8,
    CASE WHEN $8 IS NOT NULL
        THEN (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $9
        ELSE NULL
    END
)
ON CONFLICT (saga_id) DO NOTHING;
