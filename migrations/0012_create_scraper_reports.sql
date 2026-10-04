-- Per-run debug reports for `domains/scrapers/reports` — every log call,
-- HTTP request/response, progress update and input prompt recorded during
-- one scraper run (see `host::report::Recorder`), ported from
-- `demo-rust-fyde`'s `report.rs`. One row per run; rows are never updated,
-- only inserted once the run finishes.
CREATE TABLE IF NOT EXISTS scraper_reports (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scraper_name TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL,
    entries TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS scraper_reports_scraper_name ON scraper_reports (scraper_name);
