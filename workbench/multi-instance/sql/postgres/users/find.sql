SELECT id, organization_id, email, name, role, api_token, created_at
FROM workbench_users
WHERE id = $1 AND organization_id = $2;
