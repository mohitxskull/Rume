INSERT INTO rume_sessions (
    id, user_id, tenant_id, roles_json, permissions_json, data_json, expires_at, created_at
)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
ON CONFLICT (id) DO UPDATE SET
    user_id = EXCLUDED.user_id,
    tenant_id = EXCLUDED.tenant_id,
    roles_json = EXCLUDED.roles_json,
    permissions_json = EXCLUDED.permissions_json,
    data_json = EXCLUDED.data_json,
    expires_at = EXCLUDED.expires_at;
