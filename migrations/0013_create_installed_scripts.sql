-- Scripts installed by the authenticated user for `domains/scripts`,
-- materialized locally from consumed `ScriptInstalled` changelog events.
-- `script` is the full script (JSON-encoded `Script`) and `parameters` the
-- JSON object of values it was installed with. One row per script id,
-- fully replaced if the same script is installed again.
CREATE TABLE IF NOT EXISTS installed_scripts (
    script_id TEXT PRIMARY KEY,
    script TEXT NOT NULL,
    parameters TEXT NOT NULL,
    installed_at INTEGER NOT NULL DEFAULT (unixepoch())
);
