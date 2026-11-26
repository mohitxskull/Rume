UPDATE rume_tenants
SET active = FALSE
WHERE id = $1;
