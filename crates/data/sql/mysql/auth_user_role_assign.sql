INSERT IGNORE INTO rume_user_roles (user_id, tenant_id, role_id)
SELECT ?, ?, r.id
FROM rume_roles r
WHERE r.id = ? AND (r.tenant_id = '' OR r.tenant_id = ?);
