SELECT id, slug, metadata_json
FROM rume_tenants
WHERE id = ? AND active = 1;
