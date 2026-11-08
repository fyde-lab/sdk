-- The embeddings of each cached document's text (`documents::Indexer`), one
-- row per chunk: `start`/`end` are the chunk's byte range in the text it was
-- cut from, `model` the model that embedded it and `vector` its embedding,
-- 384 little-endian f32. Written in the same transaction as the document,
-- so a cached document is always indexed; a document with no text has no
-- rows. Deleted with their document, and wiped with every other table on
-- logout.
CREATE TABLE IF NOT EXISTS document_embeddings (
    document_id TEXT NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    chunk INTEGER NOT NULL,
    start INTEGER NOT NULL,
    "end" INTEGER NOT NULL,
    model TEXT NOT NULL,
    vector BLOB NOT NULL,
    PRIMARY KEY (document_id, chunk)
);
