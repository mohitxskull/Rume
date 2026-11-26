SELECT id, slug, metadata_json
FROM rume_tenants
WHERE slug = ? AND active = 1;
