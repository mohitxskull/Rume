INSERT INTO rume_user_roles (user_id, tenant_id, role_id)
SELECT $1, $2, r.id
FROM rume_roles r
WHERE r.id = $3 AND (r.tenant_id = '' OR r.tenant_id = $4)
ON CONFLICT (user_id, tenant_id, role_id) DO NOTHING;
