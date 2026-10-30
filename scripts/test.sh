#!/usr/bin/env bash
# fmt, clippy and tests in the ERPCons build image, against a local Postgres 17 container.
#   scripts/test.sh                 whole workspace
#   scripts/test.sh -p office-api   extra arguments go to `cargo test`
# Cargo never runs on the Windows host: Smart App Control blocks freshly built test binaries.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
# The workspace is the nearest directory above holding erpcons-office: the parent for the usual
# checkout, further up for a worktree such as tmp/collab-server.
ws=$(dirname "$repo")
while [ ! -f "$ws/erpcons-office/build/run.sh" ] && [ "$ws" != "$(dirname "$ws")" ]; do
  ws=$(dirname "$ws")
done
[ -f "$ws/erpcons-office/build/run.sh" ] || { echo "no erpcons-office/build/run.sh above $repo" >&2; exit 1; }
rel=${repo#"$ws"/}
pg=erpcons-office-pg-test
pg_port=55432

docker info >/dev/null 2>&1 || { echo "Docker is not running" >&2; exit 1; }

if ! docker container inspect "$pg" >/dev/null 2>&1; then
  docker run -d --name "$pg" -e POSTGRES_PASSWORD=test -p "127.0.0.1:$pg_port:5432" postgres:17 >/dev/null
elif [ "$(docker container inspect -f '{{.State.Running}}' "$pg")" != true ]; then
  docker start "$pg" >/dev/null
fi
for _ in $(seq 60); do
  docker exec "$pg" pg_isready -U postgres -q && break
  sleep 1
done
docker exec "$pg" pg_isready -U postgres -q || { echo "Postgres did not become ready" >&2; exit 1; }

# The build container reaches the host-published port through host.docker.internal (Docker Desktop).
exec bash "$ws/erpcons-office/build/run.sh" bash -c "
  cd /ws/$rel &&
  export CARGO_TARGET_DIR=/target/wt/erpcons-office-server CARGO_BUILD_JOBS=\${CARGO_BUILD_JOBS:-4} \
         TEST_DATABASE_URL=postgres://postgres:test@host.docker.internal:$pg_port/postgres &&
  cargo fmt --all --check &&
  cargo clippy --workspace --all-targets --locked -- -D warnings &&
  cargo test --workspace --locked $*
"
