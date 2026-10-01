ALTER TABLE documents RENAME COLUMN subject TO subjects;
UPDATE documents SET subjects = CASE WHEN subjects = '' THEN '[]' ELSE json_array(subjects) END;
