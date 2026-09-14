-- Local, unencrypted documents saved by `DocumentsClient::save`, as opposed
-- to `DocumentsClient::upload` which sends encrypted documents to the
-- server.
CREATE TABLE documents (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    content_type TEXT NOT NULL,
    content BLOB NOT NULL,
    created_at INTEGER NOT NULL
);
