SELECT DISTINCT rp.permission
FROM rume_role_permissions rp
INNER JOIN rume_user_roles ur ON rp.role_id = ur.role_id
WHERE ur.user_id = ? AND ur.tenant_id = ?
ORDER BY rp.permission ASC;
