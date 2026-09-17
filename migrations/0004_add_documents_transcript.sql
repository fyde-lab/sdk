-- Plaintext transcript of a document's PDF text content, extracted on
-- upload and decrypted back out of its encrypted metadata on download.
ALTER TABLE documents ADD COLUMN transcript TEXT NOT NULL DEFAULT '';
