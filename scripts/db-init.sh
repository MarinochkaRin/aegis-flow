#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v docker >/dev/null 2>&1; then
    echo 'Docker is required for this local PostgreSQL milestone.' >&2
    exit 1
fi

docker compose -f compose.postgres.yml up -d --wait
# For a NEW empty database ONLY. Migrations are not designed for repeated execution.
for migration in db/migrations/*.sql; do
    echo "Applying $migration"
    docker compose -f compose.postgres.yml exec -T postgres \
        psql -X -v ON_ERROR_STOP=1 -U aegis_flow -d aegis_flow \
        < "$migration"
done
printf '\nDatabase initialized. Run: bash scripts/db-test.sh\n'
