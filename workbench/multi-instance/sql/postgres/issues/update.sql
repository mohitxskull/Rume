UPDATE workbench_issues
SET status = $1, priority = $2, assignee_id = $3, updated_at = $4
WHERE id = $5 AND tenant_id = $6;
