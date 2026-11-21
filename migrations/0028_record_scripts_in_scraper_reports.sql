-- A scraper run's report now also records the script that ran, as the
-- server's `scripts.v1` lists it: its id and the version it ran at, both
-- NULL when the run wasn't of an installation (`run` with a bare script
-- source) and for reports kept before.
ALTER TABLE scraper_reports ADD COLUMN script_id TEXT;
ALTER TABLE scraper_reports ADD COLUMN script_version INTEGER;
