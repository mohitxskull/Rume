DELETE FROM rume_user_roles
WHERE user_id = $1 AND tenant_id = $2 AND role_id = $3;
