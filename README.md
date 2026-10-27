# erpcons-office-server

Backend of the ERPCons Office online tier: documents saved online for signed-in users. It serves `/api/v1` on the
site's own origin (`office.erpcons.vn`, Apache reverse proxy to `127.0.0.1:8710`). The free tier never calls it.

- Contract: [`docs/openapi.yaml`](docs/openapi.yaml) (source of truth for paths, error body and codes).
- Plan and decisions: `erpcons-workspace/tmp/briefs/office-online-v1/plan.md`, ADR `online-tier.md` in `erpcons-office`.
- Status: milestone 1, the skeleton. `GET /api/v1/health` and `GET /api/v1/me`, plus the pieces the document API
  (milestone 2) builds on.

## Layout

| Path | Role |
|---|---|
| `apps/server` | Binary: configuration from the environment, start-up (migrations), graceful shutdown |
| `crates/api` | axum router, `Error` body, request id, bearer auth, per-IP and per-actor rate limits, body limit, audit writer, tracing |
| `crates/documents` | Document service: `authorize` (deny by default, owner only), quota rules |
| `crates/identity` | `IdentityVerifier`; `MeEndpointVerifier` asks PI `GET /v1/me` and caches 60 s by token hash |
| `crates/storage` | `StorageAdapter`; `FsStorage` stores objects by sha256 (temp file, fsync, verify, rename) |
| `crates/db` | sqlx Postgres pool, migrations in `crates/db/migrations/` (run at start-up), queries |
| `deploy/` | `compose.yaml` (Postgres 17 + server) and `.env.example` |

## Configuration

| Variable | Default | |
|---|---|---|
| `DATABASE_URL` | required | Postgres URL |
| `ERPCONS_DATA_DIR` | required (`/data` in the image) | Object store root |
| `PI_BASE_URL` | required | PI Platform API base, without `/v1` |
| `ERPCONS_BIND` | `127.0.0.1:8710` (`0.0.0.0:8710` in the image) | Listen address |
| `ERPCONS_DB_MAX_CONNECTIONS` / `ERPCONS_DB_ACQUIRE_TIMEOUT_SECS` | `10` / `5` | Pool |
| `ERPCONS_PI_TIMEOUT_MS` | `5000` | Timeout of the `/v1/me` call |
| `ERPCONS_QUOTA_BYTES` / `ERPCONS_MAX_FILE_BYTES` | 1 GiB / 100 MiB | Per actor / per file |
| `ERPCONS_MAX_BODY_BYTES` | per-file limit + 1 MiB | Request body limit |
| `ERPCONS_RATE_IP_BURST` / `ERPCONS_RATE_IP_PER_SEC` | `300` / `30` | Per client address |
| `ERPCONS_RATE_ACTOR_BURST` / `ERPCONS_RATE_ACTOR_PER_SEC` | `120` / `10` | Per actor |
| `ERPCONS_TRUST_FORWARDED_FOR` | `true` | Client address from the last `X-Forwarded-For` entry (Apache) |
| `RUST_LOG` | `info,sqlx=warn` | Log filter |

## Tests

Cargo runs in the ERPCons build image, never on the Windows host (Smart App Control blocks new test binaries):

```sh
bash scripts/test.sh                 # fmt --check, clippy -D warnings, all tests
bash scripts/test.sh -p office-api   # extra arguments go to cargo test
```

The script starts (or reuses) the container `erpcons-office-pg-test` (`postgres:17`, `127.0.0.1:55432`) and sets
`TEST_DATABASE_URL`. Every database test creates its own database and drops it, so tests run in parallel. Without
`TEST_DATABASE_URL` those tests print `skipped` and pass. PI is an in-process `pi_mock::Server` (`erpcons-account`,
tag `v0.1.1`).

Licences and advisories: `cargo deny check` (permissive licences only, as in the forks).

## Image and deploy

```sh
docker build -t erpcons-office-server:$(git rev-parse --short HEAD) .
```

Static musl binary on `distroless/static`, user 65532. `deploy/compose.yaml` runs it with Postgres 17 (2 GB memory
limit each, `restart: unless-stopped`, server published on `127.0.0.1:8710` only, Postgres not published, named
volumes `pgdata` and `objects`). Secrets live in `deploy/.env`, which is never committed.
