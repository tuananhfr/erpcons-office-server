-- Folder names are unique per parent ignoring case and Unicode normalization (owner decision 27/10).
-- `name_key` is computed by the server (trim, NFC, then Unicode lowercase); the database only compares it.

ALTER TABLE folders ADD COLUMN name_key TEXT;

-- No folder existed before this migration (milestone 1 had no folder API); this only keeps the
-- migration valid on any database. Postgres' lower() is close to, not the same as, the server's key.
UPDATE folders SET name_key = lower(normalize(btrim(name), NFC));

ALTER TABLE folders
    ALTER COLUMN name_key SET NOT NULL,
    ADD CONSTRAINT folders_name_key_length CHECK (char_length(name_key) BETWEEN 1 AND 1024);

DROP INDEX folders_active_name_key;

CREATE UNIQUE INDEX folders_active_name_key_ci
    ON folders (owner_id, parent_id, name_key) NULLS NOT DISTINCT
    WHERE state = 'ACTIVE';
