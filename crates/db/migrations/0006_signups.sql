-- Self sign-ups on the site (office-licensing plan, part D): an address is verified by mail first, then a
-- background job asks PI to create (or reuse) the PI ID account. Office keeps no account of its own.
CREATE TABLE signups (
    id                UUID PRIMARY KEY,
    -- Trimmed, NFC, lowercase ASCII (PI refuses other addresses).
    email             TEXT NOT NULL,
    display_name      TEXT NOT NULL,
    -- `$2y$` bcrypt of the password, handed to PI; never the password itself.
    password_hash     TEXT,
    state             TEXT NOT NULL CHECK (state IN ('pending', 'verified', 'registered', 'failed')),
    -- sha256 (hex) of the one valid verification token, issued when its mail is sent.
    token_sha256      TEXT UNIQUE,
    token_expires_at  TIMESTAMPTZ,
    submitted_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    verified_at       TIMESTAMPTZ,
    push_attempts     INT NOT NULL DEFAULT 0,
    push_next_at      TIMESTAMPTZ,
    finished_at       TIMESTAMPTZ,
    -- Once PI has the account, or the sign-up ended, neither the hash nor a token may remain.
    CHECK (state IN ('pending', 'verified') OR (password_hash IS NULL AND token_sha256 IS NULL)),
    CHECK (state <> 'verified' OR (token_sha256 IS NULL AND push_next_at IS NOT NULL))
);

-- One open sign-up per address; finished ones do not block signing up again.
CREATE UNIQUE INDEX signups_open_email_idx ON signups (email) WHERE state IN ('pending', 'verified');
CREATE INDEX signups_push_due_idx ON signups (push_next_at) WHERE state = 'verified';
CREATE INDEX signups_pending_idx ON signups (submitted_at) WHERE state = 'pending';
CREATE INDEX signups_finished_idx ON signups (finished_at) WHERE state IN ('registered', 'failed');

-- The sign-up caps count verification mails by kind (per address, whole server).
CREATE INDEX mail_outbox_kind_idx ON mail_outbox (kind, created_at);
