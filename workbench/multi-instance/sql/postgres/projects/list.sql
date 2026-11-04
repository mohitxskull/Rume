SELECT id, tenant_id, name, description, created_at
FROM workbench_projects
WHERE tenant_id = $1
ORDER BY created_at DESC;
