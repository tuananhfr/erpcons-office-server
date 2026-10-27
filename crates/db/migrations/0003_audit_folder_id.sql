-- Folder events name their folder, as document events name their document. No foreign key: audit rows
-- outlive the folders they mention (the purge job deletes expired folders).

ALTER TABLE audit_events ADD COLUMN folder_id UUID;

CREATE INDEX audit_events_folder_idx ON audit_events (folder_id, at) WHERE folder_id IS NOT NULL;
