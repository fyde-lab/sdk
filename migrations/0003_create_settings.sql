-- Generic key/value store for user-facing settings and internal SDK
-- state (e.g. the session auth token, the local master key, the
-- changelog consumption offset).
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
