-- Lets `upload` and the changelog consume job look a document up by the
-- checksum of its content, to detect one that's already cached locally.
CREATE INDEX IF NOT EXISTS documents_checksum ON documents (checksum);
