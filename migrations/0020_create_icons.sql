-- A cache of the icons (`icons` domain) fetched from or created on the
-- server, keyed by their id. An icon never changes once created, so a
-- cached one never goes stale. Wiped with every other table on logout.
CREATE TABLE IF NOT EXISTS icons (
    id TEXT PRIMARY KEY NOT NULL,
    content BLOB NOT NULL
);
