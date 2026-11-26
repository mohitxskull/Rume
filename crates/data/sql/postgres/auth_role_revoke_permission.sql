DELETE FROM rume_role_permissions
WHERE role_id = $1 AND permission = $2;
