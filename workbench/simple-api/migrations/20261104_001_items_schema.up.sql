CREATE TABLE IF NOT EXISTS items (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    description TEXT,
    price_cents INTEGER NOT NULL,
    created_at BIGINT NOT NULL
);
