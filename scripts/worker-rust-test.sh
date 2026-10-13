#!/usr/bin/env bash
# Worker integration tests against a disposable database on local dev Compose.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v cargo >/dev/null || { echo 'cargo is required' >&2; exit 1; }
command -v docker >/dev/null || { echo 'docker is required' >&2; exit 1; }
[[ -f compose.postgres.yml && -f db/migrations/0001_init.sql ]] || {
  echo 'Run from an Aegis Flow repo with Compose and db/migrations/0001_init.sql' >&2
  exit 1
}

docker compose -f compose.postgres.yml up -d postgres >/dev/null
printf 'Waiting for PostgreSQL'
for _ in $(seq 1 40); do
  if docker compose -f compose.postgres.yml exec -T postgres \
      pg_isready -U aegis_flow -d aegis_flow >/dev/null 2>&1; then
    echo ' ready'
    break
  fi
  printf '.'
  sleep 1
done
if ! docker compose -f compose.postgres.yml exec -T postgres \
    pg_isready -U aegis_flow -d aegis_flow >/dev/null 2>&1; then
  echo >&2
  echo 'PostgreSQL did not become ready' >&2
  exit 1
fi

TEST_DB="aegis_worker_$(date +%s)_${RANDOM}${RANDOM}"
[[ "$TEST_DB" =~ ^aegis_worker_[0-9_]+$ ]] || {
  echo 'Unsafe test database name' >&2
  exit 1
}
created=0
cleanup() {
  if [[ "$created" -eq 1 ]]; then
    docker compose -f compose.postgres.yml exec -T postgres \
      psql -X -v ON_ERROR_STOP=1 -U aegis_flow -d postgres \
      -c "DROP DATABASE IF EXISTS \"$TEST_DB\" WITH (FORCE)" >/dev/null \
      && echo "Temporary worker test database removed: $TEST_DB" \
      || echo "WARNING: clean up test database manually: $TEST_DB" >&2
  fi
}
trap cleanup EXIT

docker compose -f compose.postgres.yml exec -T postgres \
  psql -X -v ON_ERROR_STOP=1 -U aegis_flow -d postgres \
  -c "CREATE DATABASE \"$TEST_DB\"" >/dev/null
created=1
echo "Temporary worker test database created: $TEST_DB"

docker compose -f compose.postgres.yml exec -T postgres \
  psql -X -v ON_ERROR_STOP=1 -U aegis_flow -d "$TEST_DB" \
  < db/migrations/0001_init.sql >/dev/null

AEGIS_TEST_DATABASE_URL="host=127.0.0.1 port=55432 user=aegis_flow password=aegis_flow_dev_only dbname=$TEST_DB" \
  cargo test -p flow-worker --test worker_pg -- --nocapture
