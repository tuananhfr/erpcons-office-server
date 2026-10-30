# CLAUDE.md

Backend of the ERPCons Office online tier (`/api/v1`). Read `README.md` for layout and configuration; the API contract
is `docs/openapi.yaml`. The approved plans are `erpcons-workspace/tmp/briefs/office-online-v1/plan.md` (phase 1) and
`tmp/briefs/office-online-v2/plan.md` (phase 2: versions, sharing).

## Commands

- Everything cargo runs in the build image: `bash scripts/test.sh [cargo test args]` (fmt, clippy `-D warnings`,
  tests against Postgres 17). Never run cargo on the Windows host.
- One crate: `bash scripts/test.sh -p office-storage`. Target dir: `/target/wt/erpcons-office-server` in the
  `erpcons-target` volume.
- `cargo deny check` before adding a dependency.

## Rules

- **No PII or secrets in logs or audit**: never log titles, folder or display names, bytes, tokens or `DATABASE_URL`.
  Spans carry method, route template and request id only. `privacy.rs` checks this at TRACE.
- **Migrations are add-only**: never edit or delete a migration once merged; add a new file in `crates/db/migrations/`.
  Use runtime queries (`sqlx::query*`), not the compile-time macros.
- **Auth is local**: `JwtVerifier` checks PI's EdDSA access tokens against PI's JWKS (`alg`, `typ`, signature, `iss`,
  `exp`/`nbf`/`iat`, `aud`, `azp` allow list, `sub`); never relax a check to make a token pass. `actor_id` = PI `sub`.
  A request never fails because PI's userinfo (display name) is down.
- **404 unless visible**: anything the caller may not see (no role, or a document its owner trashed) answers
  `NOT_FOUND`; only a caller who can see a document but whose role does not allow the action gets 403 `FORBIDDEN`.
  Every access decision goes through `office_documents::authorize`, with the role read from the database on every
  request (no cache). Quota and locks of a shared document are its owner's, never the caller's.
- **Emails and link tokens are PII/secrets**: emails live only in `actor_emails`, `document_acl` and `mail_outbox`;
  link tokens as sha256 (lookups) plus a copy sealed with AES-256-GCM under `ERPCONS_LINK_KEY` (shown to sharers
  again), never in the clear. Neither reaches logs or audit rows.
- **Sign-up secrets**: a password exists on the server only as the `$2y$` bcrypt hash in `signups`, dropped (a
  check constraint) once PI answered for good; a verification token only as sha256, issued by
  `Signups::issue` when its mail is sent, never in `mail_outbox`. An unverified address never reaches PI, and every
  accepted `POST /signup` gets the same answer whatever the address's state.
- **Durable write order** (plan §4): temp file, fsync, verify sha256, rename, DB transaction (version +
  `current_version_id` + `etag`), commit, then 200. Never report success before the commit. A request that fails
  after its put calls `Documents::discard` (deletes the bytes under `locks::object` unless deduplicated or referenced);
  the orphan sweep only backs up crashes.
- **Deleting a version** (owner, never the current one) removes its row; its object becomes `DELETE_PENDING` only
  when no live version uses it, and the request deletes the bytes after the commit, else the purge job does. The
  comment in migration 0001 ("only a purge may delete them") predates this.
- **Lock order** in a transaction: actor row (the document owner's), then `locks::tree` (owner), then
  `locks::object` (sha256), then document rows, then `locks::recipients` (the sharer's new-address cap), then
  `locks::mail_caps` (the invite mail caps). A mutation writes its
  audit row inside its own transaction.
- **Intake rules are shared with the site**: change `crates/documents/tests/intake_vectors.json` together with the
  site's `intake-contract.ts`; the vector test fails when they drift.
- Fail points (`FailPoint`) exist for tests only (`testing` feature); never arm them elsewhere.
- Errors use the contract's `Error` body via `ApiError`; codes must match `docs/openapi.yaml` (a unit test checks).
- One file, one responsibility; entry files (`main.rs`, `lib.rs`) only compose. Comments explain why, in English,
  1–2 lines.
- Pin dependency versions in the workspace `Cargo.toml`; permissive licences only (`deny.toml`).
