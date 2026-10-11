-- Full-text index over every searchable field of `documents`, queried by
-- `documents::Service::search`. Contentless (`content=''`), so the plaintext
-- transcript isn't stored a second time: only the tokenized index is kept,
-- and matching rows are read back from `documents` itself.
-- `remove_diacritics 2` lets "echeance" match "échéance".
--
-- `documents` has no integer key to use as this table's rowid (its implicit
-- rowid may be renumbered by `VACUUM`), hence `documents_fts_rowid_map`.
--
-- Deleted/updated rows leave their old tokens in the index's segments
-- until they're merged, so `SqliteClient::wipe` clears this table with
-- FTS5's `'delete-all'` command rather than relying on the triggers below.
CREATE VIRTUAL TABLE documents_fts USING fts5(
    name,
    original_name,
    transcript,
    type,
    source_category,
    source_sub_category,
    subjects,
    purpose,
    content = '',
    contentless_delete = 1,
    tokenize = 'unicode61 remove_diacritics 2'
);

-- Maps each `documents_fts` rowid to the `documents` row it indexes.
CREATE TABLE documents_fts_rowid_map (
    rowid INTEGER PRIMARY KEY,
    document_id TEXT NOT NULL UNIQUE
);

INSERT INTO documents_fts_rowid_map (document_id) SELECT id FROM documents;

INSERT INTO documents_fts (rowid, name, original_name, transcript, type, source_category, source_sub_category, subjects, purpose)
SELECT m.rowid, d.name, d.original_name, d.transcript, d.type, d.source_category, d.source_sub_category, d.subjects, d.purpose
FROM documents d
JOIN documents_fts_rowid_map m ON m.document_id = d.id;

CREATE TRIGGER documents_fts_after_insert AFTER INSERT ON documents BEGIN
    INSERT INTO documents_fts_rowid_map (document_id) VALUES (new.id);
    INSERT INTO documents_fts (rowid, name, original_name, transcript, type, source_category, source_sub_category, subjects, purpose)
    VALUES (
        (SELECT rowid FROM documents_fts_rowid_map WHERE document_id = new.id),
        new.name, new.original_name, new.transcript, new.type, new.source_category, new.source_sub_category, new.subjects, new.purpose
    );
END;

CREATE TRIGGER documents_fts_after_update AFTER UPDATE ON documents BEGIN
    UPDATE documents_fts
    SET name = new.name,
        original_name = new.original_name,
        transcript = new.transcript,
        type = new.type,
        source_category = new.source_category,
        source_sub_category = new.source_sub_category,
        subjects = new.subjects,
        purpose = new.purpose
    WHERE rowid = (SELECT rowid FROM documents_fts_rowid_map WHERE document_id = old.id);
END;

CREATE TRIGGER documents_fts_after_delete AFTER DELETE ON documents BEGIN
    DELETE FROM documents_fts
    WHERE rowid = (SELECT rowid FROM documents_fts_rowid_map WHERE document_id = old.id);
    DELETE FROM documents_fts_rowid_map WHERE document_id = old.id;
END;
