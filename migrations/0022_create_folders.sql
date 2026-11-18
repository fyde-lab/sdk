-- The vault's folders for `domains/folders`, materialized locally from
-- consumed `SaveFolder`/`DeleteFolder` changelog events. `parent_id` is
-- NULL for a top-level folder; `color`/`icon` are set on top-level folders
-- only (their `FolderColor`/`FolderIcon` names).
CREATE TABLE IF NOT EXISTS folders (
    id TEXT PRIMARY KEY,
    parent_id TEXT,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    color TEXT,
    icon TEXT
);

CREATE INDEX IF NOT EXISTS folders_parent_id ON folders (parent_id);
