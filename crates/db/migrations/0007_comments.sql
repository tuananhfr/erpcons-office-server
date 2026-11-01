-- Anchored comments (plan v2 §6, plan-m5). Add-only: the previous release never reads these tables. Bodies are
-- personal data: never logged or audited. A purge deletes a document's threads (and so their comments).

CREATE TABLE comment_threads (
    id                   UUID PRIMARY KEY,
    document_id          UUID NOT NULL REFERENCES documents (id),
    -- Where the editor placed it: region id, position in it and a soft anchor. Opaque to the server.
    anchor               JSONB NOT NULL CHECK (jsonb_typeof(anchor) = 'object' AND octet_length(anchor::text) <= 16384),
    -- The stable region id of the anchor, when it has one (plan-m3).
    region               TEXT CHECK (octet_length(region) BETWEEN 1 AND 256),
    -- The version the anchor was taken on.
    created_version_seq  INT NOT NULL CHECK (created_version_seq >= 1),
    -- `orphaned`: the editor could not find the anchor again; still shown, under "no longer placed".
    status               TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'resolved', 'orphaned')),
    created_by           TEXT NOT NULL REFERENCES actors (id),
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_by          TEXT REFERENCES actors (id),
    resolved_at          TIMESTAMPTZ,
    CHECK ((status = 'resolved') = (resolved_at IS NOT NULL)),
    CHECK ((resolved_by IS NULL) = (resolved_at IS NULL))
);

-- Listing by document (ids are UUIDv7, so by creation) and by status.
CREATE INDEX comment_threads_document_idx ON comment_threads (document_id, id);
CREATE INDEX comment_threads_status_idx ON comment_threads (document_id, status, id);
-- New threads of a document since a time (the batched mail).
CREATE INDEX comment_threads_created_idx ON comment_threads (document_id, created_at);

CREATE TABLE comments (
    id          UUID PRIMARY KEY,
    thread_id   UUID NOT NULL REFERENCES comment_threads (id) ON DELETE CASCADE,
    author_id   TEXT NOT NULL REFERENCES actors (id),
    -- NULL once deleted: the place in the thread stays, the words do not.
    body        TEXT CHECK (char_length(body) BETWEEN 1 AND 10000),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    edited_at   TIMESTAMPTZ,
    deleted_at  TIMESTAMPTZ,
    CHECK ((deleted_at IS NULL) = (body IS NOT NULL))
);

CREATE INDEX comments_thread_idx ON comments (thread_id, id);

-- A comment mail not yet taken by the sender covers what happens until then (one per recipient and thread, or per
-- document for new threads).
CREATE INDEX mail_outbox_comment_pending_idx ON mail_outbox (kind, (payload->>'recipient_id'), (payload->>'document_id'))
    WHERE sent_at IS NULL AND failed_at IS NULL AND attempts = 0 AND kind IN ('comment_reply', 'comment_threads');
