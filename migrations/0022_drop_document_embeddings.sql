-- Documents are no longer embedded on device: their search is full-text
-- only (`documents_fts`), so the embeddings cached by
-- `0021_create_document_embeddings` are dropped.
DROP TABLE IF EXISTS document_embeddings;
