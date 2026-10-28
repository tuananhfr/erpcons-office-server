-- The audit retention job deletes by age; without this index each run scans the whole table.

CREATE INDEX audit_events_at_idx ON audit_events (at);
