SELECT id, organization_id, email, name, role
FROM workbench_users
WHERE api_token = $1;
