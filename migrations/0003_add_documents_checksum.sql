-- Hex-encoded SHA-256 checksum of a document's content, computed on upload
-- and decrypted back out of its encrypted metadata on download.
ALTER TABLE documents ADD COLUMN checksum TEXT NOT NULL DEFAULT '';
