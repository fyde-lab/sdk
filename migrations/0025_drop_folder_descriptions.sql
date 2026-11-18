-- Folders no longer carry a description: documents are filed by what
-- their folders already hold. Dropped here rather than out of
-- 0022_create_folders.sql, which databases had already applied: sqlx
-- refuses to start on a migration changed after it was applied.
ALTER TABLE folders DROP COLUMN description;
