SELECT permission
FROM rume_role_permissions
WHERE role_id = $1
ORDER BY permission ASC;
