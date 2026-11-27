SELECT id, user_id, tenant_id, roles_json, permissions_json, data_json, expires_at, created_at
FROM rume_sessions
WHERE id = $1;
