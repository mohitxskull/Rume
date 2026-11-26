INSERT INTO rume_user_roles (user_id, tenant_id, role_id)
VALUES ($1, $2, $3)
ON CONFLICT (user_id, tenant_id, role_id) DO NOTHING;
