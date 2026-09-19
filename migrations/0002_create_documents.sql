-- Local, unencrypted documents saved by `DocumentsClient::save`, as opposed
-- to `DocumentsClient::upload` which sends encrypted documents to the
-- server.
CREATE TABLE IF NOT EXISTS documents (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    content_type TEXT NOT NULL,
    content BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    -- Hex-encoded SHA-256 checksum of the document's content, computed on
    -- upload and decrypted back out of its encrypted metadata on download.
    checksum TEXT NOT NULL DEFAULT '',
    -- Plaintext transcript of the document's PDF text content, extracted on
    -- upload and decrypted back out of its encrypted metadata on download.
    transcript TEXT NOT NULL DEFAULT ''
);
