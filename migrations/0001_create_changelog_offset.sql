-- Tracks the cursor position into the server's changelog, so
-- `ChangelogClient::consume` can resume from where it left off.
-- Single-row table: `id` is pinned to 0 to enforce that.
CREATE TABLE IF NOT EXISTS changelog_offset (
    id INTEGER PRIMARY KEY CHECK (id = 0),
    offset INTEGER NOT NULL
);
