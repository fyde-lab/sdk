-- JSON-encoded map of a document's unstructured, typed attributes (see
-- `Metadata::attributes`), since SQLite has no native map type.
ALTER TABLE documents ADD COLUMN attributes TEXT NOT NULL DEFAULT '{}';
