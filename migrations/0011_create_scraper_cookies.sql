-- Per-scraper cookie jar for `domains/scrapers/cookies` — every cookie a
-- scraper run's `fyde.http` client picked up, recorded against the origin
-- (`scheme://host`) it was seen on so it can be replayed into a fresh cookie
-- jar on the next run, carrying an already-authenticated session across
-- runs. All rows for a scraper name are replaced on every run.
CREATE TABLE IF NOT EXISTS scraper_cookies (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scraper_name TEXT NOT NULL,
    origin TEXT NOT NULL,
    set_cookie TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS scraper_cookies_scraper_name ON scraper_cookies (scraper_name);
