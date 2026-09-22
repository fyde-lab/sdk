-- The file name as originally uploaded, distinct from `name` (which can be
-- renamed later via `DocumentsClient::update_name`).
ALTER TABLE documents ADD COLUMN original_name TEXT NOT NULL DEFAULT '';
