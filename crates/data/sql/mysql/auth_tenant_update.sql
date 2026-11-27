UPDATE rume_tenants
SET slug = ?,
    metadata_json = ?,
    active = 1
WHERE id = ?;
