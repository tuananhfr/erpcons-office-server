-- Phase 2 sharing (plan v2 §2). Add-only: the previous release starts on this schema and never reads these tables,
-- so a rollback only stops sharing.

-- Verified email of an actor, from PI's userinfo. Personal data: never logged or audited.
CREATE TABLE actor_emails (
    actor_id     TEXT PRIMARY KEY REFERENCES actors (id),
    -- Lowercase NFC, computed by the server.
    email_norm   TEXT NOT NULL UNIQUE CHECK (char_length(email_norm) BETWEEN 3 AND 254),
    verified_at  TIMESTAMPTZ NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE document_acl (
    id           UUID PRIMARY KEY,
    document_id  UUID NOT NULL REFERENCES documents (id),
    -- NULL while the invitation waits for an account with this verified email.
    actor_id     TEXT REFERENCES actors (id),
    -- The address invited; shown to the people who manage sharing.
    email_norm   TEXT CHECK (char_length(email_norm) BETWEEN 3 AND 254),
    role         TEXT NOT NULL CHECK (role IN ('editor', 'commenter', 'viewer')),
    granted_by   TEXT NOT NULL REFERENCES actors (id),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ,
    revoked_at   TIMESTAMPTZ,
    -- The recipient opened "Shared with me" past it; NULL marks it new.
    seen_at      TIMESTAMPTZ,
    CHECK (actor_id IS NOT NULL OR email_norm IS NOT NULL)
);

-- One permission in force per person, and one pending invitation per address, on a document.
CREATE UNIQUE INDEX document_acl_actor_key
    ON document_acl (document_id, actor_id) WHERE revoked_at IS NULL AND actor_id IS NOT NULL;
CREATE UNIQUE INDEX document_acl_email_key
    ON document_acl (document_id, email_norm) WHERE revoked_at IS NULL AND actor_id IS NULL;
CREATE INDEX document_acl_document_idx ON document_acl (document_id) WHERE revoked_at IS NULL;
CREATE INDEX document_acl_shared_idx
    ON document_acl (actor_id, created_at, id) WHERE revoked_at IS NULL AND actor_id IS NOT NULL;
CREATE INDEX document_acl_pending_idx
    ON document_acl (email_norm) WHERE revoked_at IS NULL AND actor_id IS NULL;
-- New recipients per sharer and day (a cap against probing which address belongs to an account).
CREATE INDEX document_acl_granted_idx ON document_acl (granted_by, created_at);

-- Per-document share settings and the public view link; no row means the defaults and no link.
CREATE TABLE document_share_settings (
    document_id           UUID PRIMARY KEY REFERENCES documents (id),
    editors_can_share     BOOLEAN NOT NULL DEFAULT true,
    viewers_can_download  BOOLEAN NOT NULL DEFAULT true,
    link_role             TEXT CHECK (link_role IN ('viewer')),
    -- sha256 (hex) of a 256-bit token, for looking a token up.
    link_token_hash       TEXT UNIQUE CHECK (link_token_hash ~ '^[0-9a-f]{64}$'),
    -- The token itself, AES-256-GCM under the server's ERPCONS_LINK_KEY with the document id as associated
    -- data, so sharers can copy the link again; a database copy alone does not reveal it.
    link_token_nonce      BYTEA CHECK (octet_length(link_token_nonce) = 12),
    link_token_sealed     BYTEA,
    link_created_by       TEXT REFERENCES actors (id),
    link_created_at       TIMESTAMPTZ,
    link_expires_at       TIMESTAMPTZ,
    CHECK ((link_role IS NULL) = (link_token_hash IS NULL)),
    CHECK ((link_role IS NULL) = (link_token_nonce IS NULL)),
    CHECK ((link_role IS NULL) = (link_token_sealed IS NULL))
);

-- Mail waiting for SMTP, retried with backoff for up to 24 h. `kind` has no CHECK so a later release can add kinds
-- without a rollback refusing its rows; a release that does not know a kind gives the row up.
CREATE TABLE mail_outbox (
    id          UUID PRIMARY KEY,
    kind        TEXT NOT NULL,
    -- Who caused the mail, for the per-actor daily cap.
    sender_id   TEXT REFERENCES actors (id),
    to_email    TEXT NOT NULL,
    payload     JSONB NOT NULL,
    attempts    INT NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    next_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at     TIMESTAMPTZ,
    failed_at   TIMESTAMPTZ
);

CREATE INDEX mail_outbox_due_idx ON mail_outbox (next_at) WHERE sent_at IS NULL AND failed_at IS NULL;
CREATE INDEX mail_outbox_sender_idx ON mail_outbox (sender_id, created_at);
CREATE INDEX mail_outbox_to_idx ON mail_outbox (to_email, created_at);
-- The overall cap and the cleanup of sent mail.
CREATE INDEX mail_outbox_created_idx ON mail_outbox (created_at);

-- Views through the public link are audited without an actor.
ALTER TABLE audit_events DROP CONSTRAINT audit_events_actor_kind_check;
ALTER TABLE audit_events
    ADD CONSTRAINT audit_events_actor_kind_check CHECK (actor_kind IN ('user', 'system', 'link'));
