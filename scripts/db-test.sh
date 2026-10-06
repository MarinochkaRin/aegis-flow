#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
docker compose -f compose.postgres.yml exec -T postgres \
    psql -X -v ON_ERROR_STOP=1 -U aegis_flow -d aegis_flow \
    < db/tests/smoke.sql
