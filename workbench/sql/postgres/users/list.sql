SELECT id, organization_id, email, name, role, api_token, created_at
FROM workbench_users
WHERE organization_id = $1
ORDER BY name ASC;
