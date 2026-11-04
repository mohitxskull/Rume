SELECT id, tenant_id, name, description, created_at
FROM workbench_projects
WHERE id = $1 AND tenant_id = $2;
