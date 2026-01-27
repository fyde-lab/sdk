
CREATE TABLE document (
  id BLOB NOT NULL,
  name TEXT NOT NULL,
  checksum TEXT NOT NULL,
  detected_type TEXT NOT NULL,
  size INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  transcript TEXT NOT NULL,
  file_content BLOB NOT NULL,
  file_preview BLOB NOT NULL
) STRICT;

