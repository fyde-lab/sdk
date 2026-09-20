-- Superseded by the generic `settings` table: the changelog consumption
-- offset is now persisted there (key "changelog_offset") instead of its
-- own dedicated table.
DROP TABLE IF EXISTS changelog_offset;
