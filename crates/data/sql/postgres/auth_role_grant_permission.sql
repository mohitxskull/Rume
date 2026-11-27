INSERT INTO rume_role_permissions (role_id, permission)
VALUES ($1, $2)
ON CONFLICT (role_id, permission) DO NOTHING;
