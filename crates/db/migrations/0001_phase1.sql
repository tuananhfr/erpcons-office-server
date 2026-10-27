-- Phase 1 schema (plan §4). Migrations are add-only: never edit or drop a released one.

CREATE TABLE actors (
    id            TEXT PRIMARY KEY CHECK (char_length(id) BETWEEN 1 AND 255),
    display_name  TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE folders (
    id          UUID PRIMARY KEY,
    owner_id    TEXT NOT NULL REFERENCES actors (id),
    parent_id   UUID,
    name        TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 255),
    state       TEXT NOT NULL DEFAULT 'ACTIVE' CHECK (state IN ('ACTIVE', 'TRASHED')),
    trashed_at  TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (parent_id IS DISTINCT FROM id),
    CHECK ((state = 'TRASHED') = (trashed_at IS NOT NULL)),
    UNIQUE (owner_id, id),
    -- A parent always belongs to the same owner, whatever a handler does.
    FOREIGN KEY (owner_id, parent_id) REFERENCES folders (owner_id, id)
);

-- NULLS NOT DISTINCT makes the root (parent_id NULL) one parent; trashed folders may share names.
CREATE UNIQUE INDEX folders_active_name_key
    ON folders (owner_id, parent_id, name) NULLS NOT DISTINCT
    WHERE state = 'ACTIVE';
CREATE INDEX folders_trashed_idx ON folders (owner_id, trashed_at) WHERE state = 'TRASHED';

CREATE TABLE storage_locations (
    id             UUID PRIMARY KEY,
    provider_type  TEXT NOT NULL,
    config_ref     TEXT NOT NULL,
    health         TEXT NOT NULL DEFAULT 'UNKNOWN'
                   CHECK (health IN ('UNKNOWN', 'HEALTHY', 'DEGRADED', 'UNAVAILABLE'))
);

INSERT INTO storage_locations (id, provider_type, config_ref)
VALUES ('00000000-0000-0000-0000-000000000001', 'fs', 'ERPCONS_DATA_DIR');

CREATE TABLE storage_objects (
    id                UUID PRIMARY KEY,
    location_id       UUID NOT NULL REFERENCES storage_locations (id),
    opaque_key        TEXT NOT NULL,
    sha256            TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    size              BIGINT NOT NULL CHECK (size >= 0),
    durability_state  TEXT NOT NULL DEFAULT 'DURABLE'
                      CHECK (durability_state IN ('DURABLE', 'DELETE_PENDING', 'DELETED')),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (location_id, sha256),
    UNIQUE (location_id, opaque_key)
);

CREATE TABLE documents (
    id                  UUID PRIMARY KEY,
    tenant_id           TEXT,
    owner_id            TEXT NOT NULL REFERENCES actors (id),
    folder_id           UUID,
    title               TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 255),
    kind                TEXT NOT NULL CHECK (kind IN ('word', 'sheet', 'slide', 'pdf')),
    mime                TEXT NOT NULL,
    state               TEXT NOT NULL DEFAULT 'DRAFT'
                        CHECK (state IN ('DRAFT', 'ACTIVE', 'TRASHED', 'PURGE_PENDING', 'PURGED')),
    current_version_id  UUID,
    etag                TEXT,
    trashed_at          TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A visible document always has a durable version to open.
    CHECK (state NOT IN ('ACTIVE', 'TRASHED') OR (current_version_id IS NOT NULL AND etag IS NOT NULL)),
    CHECK (state <> 'TRASHED' OR trashed_at IS NOT NULL),
    FOREIGN KEY (owner_id, folder_id) REFERENCES folders (owner_id, id)
);

CREATE INDEX documents_folder_idx ON documents (owner_id, folder_id) WHERE state = 'ACTIVE';
CREATE INDEX documents_trashed_idx ON documents (owner_id, trashed_at) WHERE state = 'TRASHED';

CREATE TABLE document_versions (
    id                    UUID PRIMARY KEY,
    document_id           UUID NOT NULL REFERENCES documents (id),
    seq                   INTEGER NOT NULL CHECK (seq >= 1),
    storage_object_id     UUID NOT NULL REFERENCES storage_objects (id),
    sha256                TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    bytes                 BIGINT NOT NULL CHECK (bytes >= 0),
    author_id             TEXT NOT NULL REFERENCES actors (id),
    author_name_snapshot  TEXT NOT NULL,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (document_id, seq),
    -- Target of the composite foreign key below.
    UNIQUE (document_id, id)
);

CREATE INDEX document_versions_object_idx ON document_versions (storage_object_id);

-- The current version must belong to the same document.
ALTER TABLE documents
    ADD CONSTRAINT documents_current_version_fk
    FOREIGN KEY (id, current_version_id) REFERENCES document_versions (document_id, id);

-- Versions are immutable (spec §05); only a purge may delete them.
CREATE FUNCTION reject_version_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'document_versions rows are immutable';
END;
$$;

CREATE TRIGGER document_versions_immutable
    BEFORE UPDATE ON document_versions
    FOR EACH ROW EXECUTE FUNCTION reject_version_update();

CREATE TABLE idempotency_keys (
    actor_id      TEXT NOT NULL REFERENCES actors (id),
    key           TEXT NOT NULL CHECK (char_length(key) BETWEEN 1 AND 100),
    request_hash  TEXT NOT NULL,
    -- NULL while the first request is still running.
    response      JSONB,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at    TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (actor_id, key)
);

CREATE INDEX idempotency_keys_expiry_idx ON idempotency_keys (expires_at);

-- No foreign keys: audit rows outlive the actors and documents they mention. No names or content.
CREATE TABLE audit_events (
    id           UUID PRIMARY KEY,
    actor_kind   TEXT NOT NULL CHECK (actor_kind IN ('user', 'system')),
    actor_id     TEXT,
    tenant_id    TEXT,
    document_id  UUID,
    action       TEXT NOT NULL,
    result       TEXT NOT NULL CHECK (result IN ('success', 'denied', 'error')),
    trace_id     TEXT NOT NULL,
    at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX audit_events_actor_idx ON audit_events (actor_id, at);
CREATE INDEX audit_events_document_idx ON audit_events (document_id, at) WHERE document_id IS NOT NULL;
