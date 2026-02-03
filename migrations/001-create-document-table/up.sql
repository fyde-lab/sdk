
CREATE TABLE IF NOT EXISTS document (
  id BLOB NOT NULL PRIMARY KEY,
  name TEXT NOT NULL,
  checksum TEXT NOT NULL,
  detected_type TEXT NOT NULL,
  size INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  transcript TEXT NOT NULL,
  file_content BLOB NOT NULL,
  file_preview BLOB NOT NULL
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS UK_document_checksum ON document(checksum);
