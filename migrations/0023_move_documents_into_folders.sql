-- Folders replace categories: drops `category` and `subcategory` from
-- `documents` for a `folder_id` (NULL for a document at the root, in
-- "À classer"), and rebuilds `documents_fts` (see `0018`) without them. The
-- triggers reference both columns, so they must go before the columns can
-- be dropped. Existing vaults aren't migrated: their documents land at the
-- root.
DROP TRIGGER documents_fts_after_insert;
DROP TRIGGER documents_fts_after_update;
DROP TRIGGER documents_fts_after_delete;
DROP TABLE documents_fts;
DROP TABLE documents_fts_rowid_map;

ALTER TABLE documents DROP COLUMN category;
ALTER TABLE documents DROP COLUMN subcategory;
ALTER TABLE documents ADD COLUMN folder_id TEXT;

CREATE INDEX IF NOT EXISTS documents_folder_id ON documents (folder_id);

CREATE VIRTUAL TABLE documents_fts USING fts5(
    name,
    original_name,
    transcript,
    type,
    content = '',
    contentless_delete = 1,
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TABLE documents_fts_rowid_map (
    rowid INTEGER PRIMARY KEY,
    document_id TEXT NOT NULL UNIQUE
);

INSERT INTO documents_fts_rowid_map (document_id) SELECT id FROM documents;

INSERT INTO documents_fts (rowid, name, original_name, transcript, type)
SELECT m.rowid, d.name, d.original_name, d.transcript, d.type
FROM documents d
JOIN documents_fts_rowid_map m ON m.document_id = d.id;

CREATE TRIGGER documents_fts_after_insert AFTER INSERT ON documents BEGIN
    INSERT INTO documents_fts_rowid_map (document_id) VALUES (new.id);
    INSERT INTO documents_fts (rowid, name, original_name, transcript, type)
    VALUES (
        (SELECT rowid FROM documents_fts_rowid_map WHERE document_id = new.id),
        new.name, new.original_name, new.transcript, new.type
    );
END;

CREATE TRIGGER documents_fts_after_update AFTER UPDATE ON documents BEGIN
    UPDATE documents_fts
    SET name = new.name,
        original_name = new.original_name,
        transcript = new.transcript,
        type = new.type
    WHERE rowid = (SELECT rowid FROM documents_fts_rowid_map WHERE document_id = old.id);
END;

CREATE TRIGGER documents_fts_after_delete AFTER DELETE ON documents BEGIN
    DELETE FROM documents_fts
    WHERE rowid = (SELECT rowid FROM documents_fts_rowid_map WHERE document_id = old.id);
    DELETE FROM documents_fts_rowid_map WHERE document_id = old.id;
END;
