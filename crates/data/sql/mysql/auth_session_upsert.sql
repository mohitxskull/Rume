INSERT INTO rume_sessions (
    id, user_id, tenant_id, roles_json, permissions_json, data_json, expires_at, created_at
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?)
ON DUPLICATE KEY UPDATE
    user_id = VALUES(user_id),
    tenant_id = VALUES(tenant_id),
    roles_json = VALUES(roles_json),
    permissions_json = VALUES(permissions_json),
    data_json = VALUES(data_json),
    expires_at = VALUES(expires_at);
