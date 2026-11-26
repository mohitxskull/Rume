INSERT INTO rume_tenants (id, slug, metadata_json, active, created_at)
VALUES (?, ?, ?, 1, ?)
ON CONFLICT (id) DO UPDATE SET
    slug = EXCLUDED.slug,
    metadata_json = EXCLUDED.metadata_json,
    active = 1;
