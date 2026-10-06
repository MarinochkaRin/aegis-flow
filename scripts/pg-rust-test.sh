#!/usr/bin/env bash
# Integration test against a disposable database on the LOCAL dev Compose instance.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
command -v cargo >/dev/null || { echo 'cargo is required' >&2; exit 1; }
command -v docker >/dev/null || { echo 'docker is required' >&2; exit 1; }
[[ -f compose.postgres.yml && -f db/migrations/0001_init.sql ]] || {
  echo 'Run from an Aegis Flow repo with Compose and the consolidated 0001 migration' >&2
  exit 1
}
TEST_DB="aegis_rust_$(date +%s)_${RANDOM}${RANDOM}"
[[ "$TEST_DB" =~ ^aegis_rust_[0-9_]+$ ]] || { echo 'Unsafe test database name' >&2; exit 1; }
created=0
cleanup() {
  if [[ "$created" -eq 1 ]]; then
    docker compose -f compose.postgres.yml exec -T postgres \
      psql -X -v ON_ERROR_STOP=1 -U aegis_flow -d postgres \
      -c "DROP DATABASE IF EXISTS \"$TEST_DB\" WITH (FORCE)" >/dev/null \
      && echo "Temporary Rust test database removed: $TEST_DB" \
      || echo "WARNING: clean up test database manually: $TEST_DB" >&2
  fi
}
trap cleanup EXIT

docker compose -f compose.postgres.yml exec -T postgres \
  psql -X -v ON_ERROR_STOP=1 -U aegis_flow -d postgres \
  -c "CREATE DATABASE \"$TEST_DB\"" >/dev/null
created=1
echo "Temporary Rust test database created: $TEST_DB"
docker compose -f compose.postgres.yml exec -T postgres \
  psql -X -v ON_ERROR_STOP=1 -U aegis_flow -d "$TEST_DB" \
  < db/migrations/0001_init.sql >/dev/null
AEGIS_TEST_DATABASE_URL="host=127.0.0.1 port=55432 user=aegis_flow password=aegis_flow_dev_only dbname=$TEST_DB" \
  cargo test -p flow-storage-pg --test pg_storage -- --nocapture
