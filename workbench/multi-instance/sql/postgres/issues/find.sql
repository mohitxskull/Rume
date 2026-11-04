SELECT id, tenant_id, project_id, title, description, status, priority, assignee_id, created_at, updated_at
FROM workbench_issues
WHERE id = $1 AND tenant_id = $2;
