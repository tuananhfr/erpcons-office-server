# CLAUDE.md

Backend of the ERPCons Office online tier (`/api/v1`). Read `README.md` for layout and configuration; the API contract
is `docs/openapi.yaml`. The approved plan is `erpcons-workspace/tmp/briefs/office-online-v1/plan.md`.

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
- **404, never 403**: anything the caller may not see answers `NOT_FOUND`. Every access decision goes through
  `office_documents::authorize`.
- **Durable write order** (plan §4): temp file, fsync, verify sha256, rename, DB transaction (version +
  `current_version_id` + `etag`), commit, then 200. Never report success before the commit. A request that fails
  after its put calls `Documents::discard` (deletes the bytes under `locks::object` unless deduplicated or referenced);
  the orphan sweep only backs up crashes.
- **Lock order** in a transaction: actor row, then `locks::tree` (owner), then `locks::object` (sha256), then document
  rows. A mutation writes its audit row inside its own transaction.
- **Intake rules are shared with the site**: change `crates/documents/tests/intake_vectors.json` together with the
  site's `intake-contract.ts`; the vector test fails when they drift.
- Fail points (`FailPoint`) exist for tests only (`testing` feature); never arm them elsewhere.
- Errors use the contract's `Error` body via `ApiError`; codes must match `docs/openapi.yaml` (a unit test checks).
- One file, one responsibility; entry files (`main.rs`, `lib.rs`) only compose. Comments explain why, in English,
  1–2 lines.
- Pin dependency versions in the workspace `Cargo.toml`; permissive licences only (`deny.toml`).
