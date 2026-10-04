-- Per-scraper session data for `domains/scrapers/session` — an arbitrary
-- JSON object a Lua scraper script reads/writes freely as `fyde.session` to
-- carry its own state between runs (cursors, "already seen" markers, ...).
-- One row per scraper name, fully replaced on every run.
CREATE TABLE IF NOT EXISTS scraper_sessions (
    scraper_name TEXT PRIMARY KEY,
    data TEXT NOT NULL
);
