INSERT INTO rume_role_permissions (role_id, permission)
VALUES (?, ?)
ON CONFLICT (role_id, permission) DO NOTHING;
