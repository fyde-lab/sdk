-- A scraper run's report now also records how the run went, so the latest
-- one of each scraper (an installation's id, for `run_installed`) tells
-- the app how its last run ended after a restart: a row is inserted as the
-- run starts (`outcome` 'running', no `entries` yet), then completed once
-- it ends ('succeeded', 'failed' with its `ErrorCode` as `error_code`, or
-- 'stopped'), with its debug log. A 'running' row whose run isn't going on
-- any more (the app was closed mid-run) reads as interrupted. `id` is the
-- run's id. Reports kept before carry none of this and are dropped (they
-- were debugging aids only): no backward compatibility.
DROP TABLE IF EXISTS scraper_reports;

CREATE TABLE scraper_reports (
    id TEXT PRIMARY KEY,
    scraper_name TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL,
    ended_at_ms INTEGER,
    outcome TEXT NOT NULL,
    error_code INTEGER,
    step TEXT,
    update_current INTEGER,
    update_total INTEGER,
    found INTEGER,
    saved INTEGER NOT NULL,
    entries TEXT NOT NULL
);

CREATE INDEX scraper_reports_scraper_name ON scraper_reports (scraper_name, started_at_ms);
