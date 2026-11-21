-- Whether a scraper run's report was uploaded to the server's `reports.v1`
-- (which links it to no user). Only finished runs of a known script
-- (`script_id` set) are uploaded, once each, in the background.
ALTER TABLE scraper_reports ADD COLUMN uploaded INTEGER NOT NULL DEFAULT 0;

CREATE INDEX scraper_reports_to_upload ON scraper_reports (uploaded, started_at_ms, id);
