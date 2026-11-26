SELECT r.id, r.tenant_id, r.name, r.description
FROM rume_roles r
INNER JOIN rume_user_roles ur ON r.id = ur.role_id
WHERE ur.user_id = $1
  AND ur.tenant_id IN ($2, '')
  AND (r.tenant_id = '' OR r.tenant_id = ur.tenant_id)
ORDER BY r.name ASC;
