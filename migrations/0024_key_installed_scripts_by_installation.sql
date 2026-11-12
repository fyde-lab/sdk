-- A user can install the same script several times, once per account on
-- its site: each installation now has its own id (`installation_id`, the
-- `InstallScript` event's subject), under which its scraper session,
-- cookies and reports are kept, and a `label` telling it apart from the
-- others. Installations made before carry neither and are dropped with
-- the state kept under their script id: no backward compatibility.
DROP TABLE IF EXISTS installed_scripts;

CREATE TABLE installed_scripts (
    installation_id TEXT PRIMARY KEY,
    script_id TEXT NOT NULL,
    label TEXT NOT NULL,
    script TEXT NOT NULL,
    parameters TEXT NOT NULL,
    installed_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX installed_scripts_script_id ON installed_scripts (script_id);

DELETE FROM scraper_sessions;
DELETE FROM scraper_cookies;
DELETE FROM scraper_reports;
