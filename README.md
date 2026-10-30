# erpcons-office-server

Backend of the ERPCons Office online tier: documents saved online for signed-in users. It serves `/api/v1` on the
site's own origin (`office.erpcons.vn`, Apache reverse proxy to `127.0.0.1:8710`). The free tier never calls it.

- Contract: [`docs/openapi.yaml`](docs/openapi.yaml) (source of truth for paths, error body and codes).
- Plan and decisions: `erpcons-workspace/tmp/briefs/office-online-v1/plan.md` (phase 1),
  `tmp/briefs/office-online-v2/plan.md` (phase 2), ADR `online-tier.md` in `erpcons-office`.
- Status: phase 1 (health, `/me`, folders, documents: upload, blank, content, save with `If-Match` and replay of a
  save whose answer was lost, versions, folder path, trash and purge on request, the purge, orphan and audit retention
  jobs), plus phase 2 milestone 1 (old versions' bytes, restoring a version, deleting an old version: owner only, bytes
  freed when no other version uses them) and the server part of milestone 2
  (sharing by email with roles, pending invitations, share settings, the public view link, "Shared with me",
  "shared with you" mail through an outbox), and self sign-up (`/signup`: verified by mail, then the PI ID account
  is created through PI's `register`, office-licensing plan part D).
- Known limit: `Idempotency-Key` stores the response after the create commits. If the server dies in between, the
  key's claim lapses after 60 s and a repeat creates a second item instead of replaying the first.

## Layout

| Path | Role |
|---|---|
| `apps/server` | Binary: configuration from the environment, start-up (migrations), background jobs, graceful shutdown |
| `crates/api` | axum router and thin handlers, `Error` body (also for framework rejections), request id, bearer auth (sampled denial audit), rate limits (per address with IPv6 by /64, per actor, token checks, concurrent uploads), body limits (64 KiB JSON), `Idempotency-Key`, tracing |
| `crates/documents` | Document service: `authorize` (deny by default; roles owner, editor, commenter, viewer, link, read from the database on every request), folders and documents, durable save (bytes of a failed request are removed at once), version restore, quota (charged to the document's owner), sharing (permissions by email, pending invitations, share settings, link tokens stored as sha256, "Shared with me"), intake checks (signature, ZIP bomb), purge on request, purge, orphan and audit retention jobs, blank templates in `templates/` |
| `crates/mail` | "Shared with you" mail: the notice queued in `mail_outbox`, its Vietnamese text, the SMTP transport (lettre, rustls) and the outbox job (retries with backoff for up to 24 h); also the two sign-up mails, whose verification token is issued by the sign-up service when the mail is sent |
| `crates/signup` | Self sign-up: form rules (shared with the site's `src/lib/signup/rules.ts`), `$2y$` bcrypt hash of the password, one-time verification tokens (sha256 only, 48 h, the latest mail's only), caps (3 mails per address, a server-wide daily cap), the job that sends verified sign-ups to PI's `POST /api/v1/service/accounts/register` with backoff for up to 7 days and then drops the hash, and the expiry of unverified sign-ups after 7 days |
| `crates/identity` | `IdentityVerifier`; `JwtVerifier` checks PI ID access tokens locally (EdDSA JWT: signature against PI's JWKS, `alg`, `typ`, `iss`, `exp`/`nbf`/`iat`, `aud`, `azp`, `sub`), caches verdicts 60 s (never past `exp`) and asks PI's `/id/userinfo` for the display name and the verified email once per actor and hour; `testing::FakePi` stands in for PI in tests |
| `crates/storage` | `StorageAdapter`; `FsStorage` stores objects by sha256 (temp file, fsync, verify, rename) |
| `crates/db` | sqlx Postgres pool, migrations in `crates/db/migrations/` (run at start-up), queries |
| `deploy/` | `compose.yaml` (Postgres 17 + server) and `.env.example` |

## Configuration

| Variable | Default | |
|---|---|---|
| `DATABASE_URL` | required | Postgres URL |
| `ERPCONS_DATA_DIR` | required (`/data` in the image) | Object store root |
| `PI_ISSUER` | required | PI ID issuer, exactly as the tokens' `iss` (PI's `pi_id_issuer`); keys come from `{PI_ISSUER}/id/jwks.json`, names from `{PI_ISSUER}/id/userinfo` |
| `ERPCONS_LINK_KEY` | required | 32 bytes in standard base64 (`openssl rand -base64 32`); seals public link tokens so sharers can copy a link again. Missing or malformed, the server does not start. Back it up with the database: under a new key old links still open, but the dialog shows them only after they are turned off and on again |
| `PI_AUDIENCES` | `pi-platform` | Comma-separated; a token's `aud` must name one |
| `PI_ALLOWED_CLIENTS` | `erpcons-office-web` | Comma-separated; a token's `azp` must be one (add `erpcons-office-desktop` when the desktop apps sync) |
| `ERPCONS_BIND` | `127.0.0.1:8710` (`0.0.0.0:8710` in the image) | Listen address |
| `ERPCONS_DB_MAX_CONNECTIONS` / `ERPCONS_DB_ACQUIRE_TIMEOUT_SECS` | `10` / `5` | Pool |
| `ERPCONS_PI_TIMEOUT_MS` | `5000` | Timeout of a JWKS fetch (a userinfo call waits at most 2 s) |
| `ERPCONS_PI_JWKS_TTL_SECS` | `300` | Age after which PI's keys are refreshed in the background; cached keys stay in use while PI is down, an unknown `kid` refetches at most every 10 s |
| `ERPCONS_QUOTA_BYTES` / `ERPCONS_MAX_FILE_BYTES` | 1 GiB / 100 MiB | Per actor / per file |
| `ERPCONS_MAX_BODY_BYTES` | per-file limit + 1 MiB | Body limit of uploads and saves; JSON bodies stop at 64 KiB (or this, if smaller) |
| `ERPCONS_RATE_IP_BURST` / `ERPCONS_RATE_IP_PER_SEC` | `300` / `30` | Per client address |
| `ERPCONS_RATE_ACTOR_BURST` / `ERPCONS_RATE_ACTOR_PER_SEC` | `120` / `10` | Per actor |
| `ERPCONS_RATE_VERIFY_BURST` / `ERPCONS_RATE_VERIFY_PER_SEC` | `60` / `5` | Per client address, token checks that need a key and a signature check (not cached, well-formed) |
| `ERPCONS_MAX_CONCURRENT_UPLOADS` | `3` (at least `1`) | Uploads and saves one actor may stream at once; more answer 429 |
| `ERPCONS_TRUST_FORWARDED_FOR` | `true` | Client address from the last `X-Forwarded-For` entry (Apache) |
| `ERPCONS_TRUSTED_PROXIES` | `127.0.0.0/8,::1` (compose: the private ranges) | Only peers in these ranges may set `X-Forwarded-For`; others are keyed by their own address |
| `ERPCONS_PURGE_INTERVAL_SECS` / `ERPCONS_ORPHAN_INTERVAL_SECS` | `3600` / `3600` | Job intervals; `0` turns a job off |
| `ERPCONS_TRASH_RETENTION_DAYS` | `30` | Trashed items stay restorable this long, then are purged |
| `ERPCONS_ORPHAN_MIN_AGE_SECS` | `600` (at least `60`) | Stored bytes with no row are deleted once this old |
| `ERPCONS_AUDIT_RETENTION_DAYS` | `180` | Audit rows older than this are deleted hourly; `0` keeps them forever |
| `ERPCONS_RATE_LINK_BURST` / `ERPCONS_RATE_LINK_PER_SEC` | `60` / `2` | Per client address, opening public view links (no sign-in) |
| `ERPCONS_SMTP_HOST` | unset | SMTP server for "shared with you" mail; unset, no mail is queued or sent and the settings below are ignored |
| `ERPCONS_SMTP_TLS` / `ERPCONS_SMTP_PORT` | `starttls` / by mode | `starttls` (587), `tls` (465) or `none` (25, a local relay only) |
| `ERPCONS_SMTP_USERNAME` / `ERPCONS_SMTP_PASSWORD` | unset | Both or neither |
| `ERPCONS_MAIL_FROM` | required with a host | `From:`, e.g. `ERPCons Office <no-reply@erpcons.vn>` |
| `ERPCONS_PUBLIC_ORIGIN` | required with a host | The site's origin for the link in the mail (`https://office.erpcons.ai`) |
| `ERPCONS_MAIL_INCLUDE_TITLE` | `false` | Put the document title in the mail |
| `ERPCONS_MAIL_DAILY_CAP` | `100` | Invite mails one account may cause per rolling 24 h; past it sharing still succeeds without mail (`mail_skipped`) |
| `ERPCONS_MAIL_GLOBAL_DAILY_CAP` | `2000` | Invite mails the whole server queues per rolling 24 h; one address also receives at most 10 a day (fixed) |
| `ERPCONS_PI_SERVICE_KEY` | unset | PI's service key for `erpconsoffice` (`X-PI-Service-Key`); with SMTP it opens self sign-up and the job that registers verified sign-ups at `{PI_ISSUER}/api/v1/service/accounts/register`. Unset (or no SMTP), `POST /signup` answers 403 `SIGNUP_CLOSED`. Never logged |
| `ERPCONS_SIGNUP_DAILY_CAP` | `100` | Sign-ups (verification mails) the whole server accepts per rolling 24 h; past it 429 `SIGNUP_DAILY_LIMIT`. One address also gets at most 3 verification mails a day (fixed, silently) |
| `ERPCONS_RATE_SIGNUP_BURST` / `ERPCONS_RATE_SIGNUP_PER_SEC` | `5` / `5/3600` | Per client address, sign-ups submitted |
| `ERPCONS_RATE_SIGNUP_VERIFY_BURST` / `ERPCONS_RATE_SIGNUP_VERIFY_PER_SEC` | `20` / `20/3600` | Per client address, verification tokens tried |
| `RUST_LOG` | `info,sqlx=warn` | Log filter |

## Tests

Cargo runs in the ERPCons build image, never on the Windows host (Smart App Control blocks new test binaries):

```sh
bash scripts/test.sh                 # fmt --check, clippy -D warnings, all tests
bash scripts/test.sh -p office-api   # extra arguments go to cargo test
```

The script starts (or reuses) the container `erpcons-office-pg-test` (`postgres:17`, `127.0.0.1:55432`) and sets
`TEST_DATABASE_URL`. Every database test creates its own database and drops it, so tests run in parallel. Without
`TEST_DATABASE_URL` those tests print `skipped` and pass. PI is `office_identity::testing::FakePi`, an in-process
stand-in that signs tokens with a fresh Ed25519 key and serves `/id/jwks.json` and `/id/userinfo` as PI's code does.

Licences and advisories: `cargo deny check` (permissive licences only, as in the forks).

The upload signature rules are shared with the site: `crates/documents/tests/intake_vectors.json` is compared with
`erpcons-office/src/lib/files/intake-contract.ts` (sibling checkout; `ERPCONS_SITE_INTAKE_CONTRACT` overrides the
path). Blank templates are made with the forks' CLIs: see `crates/documents/templates/README.md`.

## Image and deploy

```sh
docker build -t erpcons-office-server:$(git rev-parse --short HEAD) .
```

Static musl binary on `distroless/static`, user 65532. `deploy/compose.yaml` runs it with Postgres 17 (2 GB memory
limit each, `restart: unless-stopped`, server published on `127.0.0.1:8710` only, Postgres not published, named
volumes `pgdata` and `objects`). Secrets live in `deploy/.env`, which is never committed.
