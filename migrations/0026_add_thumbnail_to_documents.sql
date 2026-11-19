-- Each document's first-page thumbnail, drawn in the background once it's
-- cached: NULL until then, empty when its first page couldn't be drawn, else
-- a PNG. Plaintext-derived like `content`, and wiped with its row. When the
-- way thumbnails are drawn changes, a migration resets the column to NULL
-- so every one is drawn again.
ALTER TABLE documents ADD COLUMN thumbnail BLOB;

-- The documents still waiting for theirs, newest first, without scanning
-- the table (and every document's content along the way).
CREATE INDEX documents_without_thumbnail ON documents (created_at, id)
WHERE thumbnail IS NULL;

-- Only the columns the full-text index holds: storing a thumbnail must not
-- re-tokenize the document's whole transcript.
DROP TRIGGER documents_fts_after_update;

CREATE TRIGGER documents_fts_after_update
AFTER UPDATE OF name, original_name, transcript, type ON documents BEGIN
    UPDATE documents_fts
    SET name = new.name,
        original_name = new.original_name,
        transcript = new.transcript,
        type = new.type
    WHERE rowid = (SELECT rowid FROM documents_fts_rowid_map WHERE document_id = old.id);
END;
